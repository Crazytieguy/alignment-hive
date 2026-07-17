#!/usr/bin/env python3
"""Persist IOPub JSONL (64 MiB + one predecessor); diagnostics (1 MiB + one)."""

import argparse
import base64
import datetime
import fcntl
import hashlib
import json
import os
import secrets
import socket
import struct
import time
import urllib.parse


OUTPUT_MAX_BYTES = 64 * 1024 * 1024
DIAGNOSTIC_MAX_BYTES = 1024 * 1024
READ_TIMEOUT_SECS = 90
NEVER_CONNECTED_EXIT_SECS = 60 * 60
MAX_BACKOFF_SECS = 60


class KernelGone(Exception):
    pass


class BufferedSocket:
    def __init__(self, sock, initial=b""):
        self.sock = sock
        self.buffer = bytearray(initial)

    def recv(self, size):
        if self.buffer:
            chunk = bytes(self.buffer[:size])
            del self.buffer[:size]
            return chunk
        return self.sock.recv(size)

    def sendall(self, payload):
        self.sock.sendall(payload)

    def close(self):
        self.sock.close()


class RotatingJsonl:
    def __init__(self, path, max_bytes=OUTPUT_MAX_BYTES):
        self.path = path
        self.max_bytes = max_bytes
        repair_torn_tail(path)
        self.output = open(path, "a", encoding="utf-8", buffering=1)

    def write(self, value):
        line = json.dumps(value, separators=(",", ":")) + "\n"
        if self.output.tell() + len(line.encode("utf-8")) > self.max_bytes:
            self.output.close()
            predecessor = self.path + ".1"
            try:
                os.unlink(predecessor)
            except FileNotFoundError:
                pass
            os.replace(self.path, predecessor)
            self.output = open(self.path, "a", encoding="utf-8", buffering=1)
        self.output.write(line)
        self.output.flush()

    def close(self):
        self.output.close()


def arguments():
    parser = argparse.ArgumentParser()
    parser.add_argument("--kernel-id", required=True)
    parser.add_argument("--state-dir", required=True)
    parser.add_argument("--ws-url", default="ws://127.0.0.1:8888")
    parser.add_argument("--diagnostic-log", required=True)
    return parser.parse_args()


def repair_torn_tail(path):
    try:
        with open(path, "rb+") as output:
            output.seek(0, os.SEEK_END)
            if output.tell() == 0:
                return
            output.seek(-1, os.SEEK_END)
            if output.read(1) != b"\n":
                output.seek(0, os.SEEK_END)
                output.write(b"\n")
                output.flush()
    except FileNotFoundError:
        pass


def rotate_path(path, max_bytes):
    try:
        if os.path.getsize(path) < max_bytes:
            return
    except FileNotFoundError:
        return
    predecessor = path + ".1"
    try:
        os.unlink(predecessor)
    except FileNotFoundError:
        pass
    os.replace(path, predecessor)


def log_diagnostic(path, message):
    rotate_path(path, DIAGNOSTIC_MAX_BYTES)
    timestamp = datetime.datetime.now(datetime.timezone.utc).isoformat()
    with open(path, "a", encoding="utf-8") as output:
        output.write(f"{timestamp} {message}\n")


def retry_delay(failures):
    return min(MAX_BACKOFF_SECS, 2 ** min(failures + 1, 6))


def never_connected_expired(ever_connected, started, now):
    return not ever_connected and now - started >= NEVER_CONNECTED_EXIT_SECS


def recv_exact(sock, size):
    chunks = []
    while size:
        chunk = sock.recv(size)
        if not chunk:
            raise ConnectionError("websocket closed")
        chunks.append(chunk)
        size -= len(chunk)
    return b"".join(chunks)


def send_frame(sock, opcode, payload=b""):
    mask = secrets.token_bytes(4)
    length = len(payload)
    header = bytes([0x80 | opcode])
    if length < 126:
        header += bytes([0x80 | length])
    elif length < 65536:
        header += bytes([0x80 | 126]) + struct.pack("!H", length)
    else:
        header += bytes([0x80 | 127]) + struct.pack("!Q", length)
    masked = bytes(value ^ mask[index % 4] for index, value in enumerate(payload))
    sock.sendall(header + mask + masked)


def recv_frame(sock):
    first, second = recv_exact(sock, 2)
    opcode = first & 0x0F
    length = second & 0x7F
    if length == 126:
        length = struct.unpack("!H", recv_exact(sock, 2))[0]
    elif length == 127:
        length = struct.unpack("!Q", recv_exact(sock, 8))[0]
    mask = recv_exact(sock, 4) if second & 0x80 else None
    payload = recv_exact(sock, length)
    if mask:
        payload = bytes(value ^ mask[index % 4] for index, value in enumerate(payload))
    return bool(first & 0x80), opcode, payload


def connect(args, token):
    parsed = urllib.parse.urlparse(args.ws_url)
    if parsed.scheme != "ws":
        raise ValueError("recorder requires a ws:// localhost endpoint")
    host = parsed.hostname or "127.0.0.1"
    port = parsed.port or 80
    quoted_token = urllib.parse.quote(token, safe="")
    path = f"{parsed.path.rstrip('/')}/api/kernels/{args.kernel_id}/channels?token={quoted_token}"
    key = base64.b64encode(secrets.token_bytes(16)).decode("ascii")
    request = (
        f"GET {path} HTTP/1.1\r\n"
        f"Host: {host}:{port}\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        f"Sec-WebSocket-Key: {key}\r\n"
        "Sec-WebSocket-Version: 13\r\n"
        f"Authorization: token {token}\r\n\r\n"
    )
    sock = socket.create_connection((host, port), timeout=10)
    sock.sendall(request.encode("utf-8"))
    response = b""
    while b"\r\n\r\n" not in response:
        chunk = sock.recv(4096)
        if not chunk:
            sock.close()
            raise ConnectionError("websocket handshake closed")
        response += chunk
        if len(response) > 65536:
            sock.close()
            raise ConnectionError("oversize websocket response")
    headers, remainder = response.split(b"\r\n\r\n", 1)
    status = headers.split(b"\r\n", 1)[0]
    if b" 404 " in status:
        sock.close()
        raise KernelGone()
    if b" 101 " not in status:
        sock.close()
        raise ConnectionError(status.decode("utf-8", "replace"))
    expected = base64.b64encode(
        hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode("ascii")).digest()
    ).decode("ascii")
    if f"sec-websocket-accept: {expected}".lower().encode("ascii") not in headers.lower():
        sock.close()
        raise ConnectionError("invalid websocket accept")
    sock.settimeout(READ_TIMEOUT_SECS)
    return BufferedSocket(sock, remainder)


def record(sock, output):
    fragments = bytearray()
    fragment_opcode = None
    while True:
        final, opcode, payload = recv_frame(sock)
        if opcode == 0x8:
            raise ConnectionError("websocket closed")
        if opcode == 0x9:
            send_frame(sock, 0xA, payload)
            continue
        if opcode == 0xA:
            continue
        if opcode in (0x1, 0x2):
            fragments = bytearray(payload)
            fragment_opcode = opcode
        elif opcode == 0x0 and fragment_opcode is not None:
            fragments.extend(payload)
        else:
            continue
        if not final:
            continue
        if fragment_opcode != 0x1:
            fragments.clear()
            fragment_opcode = None
            continue
        try:
            message = json.loads(fragments.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            fragments.clear()
            fragment_opcode = None
            continue
        fragments.clear()
        fragment_opcode = None
        if message.get("channel") != "iopub":
            continue
        parent_msg_id = message.get("parent_header", {}).get("msg_id")
        msg_type = message.get("header", {}).get("msg_type")
        if not parent_msg_id or not msg_type:
            continue
        output.write(
            {
                "parent_msg_id": parent_msg_id,
                "msg_type": msg_type,
                "content": message.get("content", {}),
                "ts": datetime.datetime.now(datetime.timezone.utc).isoformat(),
            }
        )


def remove_kernel_files(output_dir, kernel_id):
    bases = [
        os.path.join(output_dir, kernel_id + ".jsonl"),
        os.path.join(output_dir, kernel_id + ".recorder.log"),
    ]
    for base in bases:
        for path in (base, base + ".1"):
            try:
                os.unlink(path)
            except FileNotFoundError:
                pass


def main():
    args = arguments()
    if not args.kernel_id or any(char not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_." for char in args.kernel_id):
        raise SystemExit("invalid kernel id")
    token = os.environ.get("REMOTE_KERNELS_JUPYTER_TOKEN")
    if not token:
        raise SystemExit("REMOTE_KERNELS_JUPYTER_TOKEN is required")
    output_dir = os.path.join(args.state_dir, "kernel-output")
    os.makedirs(output_dir, exist_ok=True)
    lock_path = os.path.join(output_dir, args.kernel_id + ".lock")
    clean_gone = False
    with open(lock_path, "a", encoding="utf-8") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return
        log_path = os.path.join(output_dir, args.kernel_id + ".jsonl")
        pid_path = os.path.join(output_dir, args.kernel_id + ".pid")
        with open(pid_path, "w", encoding="utf-8") as pid_file:
            pid_file.write(str(os.getpid()))
        output = RotatingJsonl(log_path)
        started = time.monotonic()
        ever_connected = False
        failures = 0
        try:
            while True:
                try:
                    sock = connect(args, token)
                    ever_connected = True
                    failures = 0
                    try:
                        record(sock, output)
                    finally:
                        sock.close()
                except KernelGone:
                    clean_gone = True
                    return
                except Exception as error:
                    if never_connected_expired(ever_connected, started, time.monotonic()):
                        log_diagnostic(args.diagnostic_log, f"recorder exit never-connected: {error}")
                        return
                    delay = retry_delay(failures)
                    failures += 1
                    log_diagnostic(args.diagnostic_log, f"recorder reconnect in {delay}s: {error}")
                    time.sleep(delay)
        finally:
            output.close()
            try:
                os.unlink(pid_path)
            except FileNotFoundError:
                pass
            if clean_gone:
                remove_kernel_files(output_dir, args.kernel_id)


if __name__ == "__main__":
    main()
