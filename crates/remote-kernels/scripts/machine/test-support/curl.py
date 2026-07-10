#!/usr/bin/env python3
"""Sandbox fallback for curl when local TCP sockets are prohibited."""

import json
import os
import pathlib


state = pathlib.Path(os.environ["RK_FAKE_JUPYTER_STATE"]).read_text(encoding="utf-8").strip()
print(json.dumps([{"id": "test-kernel", "execution_state": state}]))
