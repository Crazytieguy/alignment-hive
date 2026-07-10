#!/usr/bin/env python3
"""Tiny local Jupyter kernels endpoint used only by machine script tests."""

import json
import pathlib
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


PORT = int(sys.argv[1])
STATE_FILE = pathlib.Path(sys.argv[2])


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path != "/api/kernels":
            self.send_error(404)
            return
        state = STATE_FILE.read_text(encoding="utf-8").strip()
        body = json.dumps([{"id": "test-kernel", "execution_state": state}]).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, _format, *_args):
        return


ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
