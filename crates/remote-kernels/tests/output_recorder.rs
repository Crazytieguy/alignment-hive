use std::process::Command;

fn run_python(body: &str) {
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("scripts/machine/rk-output-recorder.py");
    let script_json = serde_json::to_string(&script.display().to_string()).unwrap();
    let program = format!(
        "import importlib.util\nspec = importlib.util.spec_from_file_location('rk', {script_json})\nrk = importlib.util.module_from_spec(spec)\nspec.loader.exec_module(rk)\n{body}"
    );
    let output = Command::new("python3")
        .args(["-c", &program])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "python recorder test failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn wrong_port_retry_is_bounded_and_never_connected_exits() {
    run_python(
        "assert [rk.retry_delay(i) for i in range(8)] == [2, 4, 8, 16, 32, 60, 60, 60]\nassert not rk.never_connected_expired(False, 0, 3599)\nassert rk.never_connected_expired(False, 0, 3600)\nassert not rk.never_connected_expired(True, 0, 9999)",
    );
}

#[test]
fn coalesced_handshake_remainder_precedes_socket_reads() {
    run_python(
        "import base64, hashlib, types\nkey_bytes = b'x' * 16\nkey = base64.b64encode(key_bytes).decode('ascii')\naccept = base64.b64encode(hashlib.sha1((key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode('ascii')).digest()).decode('ascii')\nresponse = ('HTTP/1.1 101 Switching Protocols\\r\\nSec-WebSocket-Accept: ' + accept + '\\r\\n\\r\\n').encode() + b'\\x81\\x02hi'\nclass Sock:\n    def __init__(self): self.timeout = None\n    def recv(self, size):\n        global response\n        chunk, response = response[:size], response[size:]\n        return chunk\n    def sendall(self, payload): pass\n    def close(self): pass\n    def settimeout(self, value): self.timeout = value\nsock = Sock()\nrk.secrets.token_bytes = lambda size: key_bytes[:size]\nrk.socket.create_connection = lambda address, timeout: sock\nstream = rk.connect(types.SimpleNamespace(ws_url='ws://127.0.0.1:1', kernel_id='k'), 'token')\nassert sock.timeout == rk.READ_TIMEOUT_SECS\nassert rk.recv_frame(stream)[2] == b'hi'",
    );
}

#[test]
fn startup_repairs_torn_recorder_line() {
    run_python(
        "import os, tempfile\nwith tempfile.TemporaryDirectory() as d:\n    path = os.path.join(d, 'k.jsonl')\n    open(path, 'wb').write(b'partial')\n    rk.repair_torn_tail(path)\n    assert open(path, 'rb').read() == b'partial\\n'",
    );
}

#[test]
fn rotation_keeps_one_predecessor_and_clean_404_removes_files() {
    run_python(
        "import os, tempfile\nwith tempfile.TemporaryDirectory() as d:\n    path = os.path.join(d, 'k.jsonl')\n    out = rk.RotatingJsonl(path, max_bytes=40)\n    out.write({'value': 'a' * 30})\n    out.write({'value': 'b' * 30})\n    out.write({'value': 'c' * 30})\n    out.close()\n    assert os.path.exists(path)\n    assert os.path.exists(path + '.1')\n    assert len([p for p in os.listdir(d) if p.startswith('k.jsonl')]) == 2\n    diag = os.path.join(d, 'k.recorder.log')\n    open(diag, 'w').write('diagnostic')\n    rk.rotate_path(diag, 3)\n    assert os.path.exists(diag + '.1')\n    open(diag, 'w').write('new')\n    rk.remove_kernel_files(d, 'k')\n    assert not any(p.startswith('k.jsonl') or p.startswith('k.recorder.log') for p in os.listdir(d))",
    );
}
