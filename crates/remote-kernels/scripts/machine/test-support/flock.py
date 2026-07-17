#!/usr/bin/env python3
"""Minimal fd-form flock used by the local harness on hosts without util-linux."""

import fcntl
import sys


nonblocking = "-n" in sys.argv[1:]
fd = int(sys.argv[-1])
flags = fcntl.LOCK_EX | (fcntl.LOCK_NB if nonblocking else 0)
try:
    fcntl.flock(fd, flags)
except BlockingIOError:
    sys.exit(1)
