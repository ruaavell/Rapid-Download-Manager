#!/usr/bin/env python3
"""Minimal Native Messaging host.

This MVP safely receives URLs and appends them to an inbox JSONL file.
The desktop UI handoff consumer is a follow-up integration; no shell command
is executed and no URL is interpolated into a command.
"""
import json
import os
import struct
import sys
from pathlib import Path
from urllib.parse import urlparse

MAX_MESSAGE = 1024 * 1024

def read_message():
    raw_len = sys.stdin.buffer.read(4)
    if len(raw_len) != 4:
        return None
    length = struct.unpack("<I", raw_len)[0]
    if length > MAX_MESSAGE:
        raise ValueError("message too large")
    raw = sys.stdin.buffer.read(length)
    if len(raw) != length:
        raise ValueError("truncated message")
    return json.loads(raw.decode("utf-8"))

def write_message(payload):
    raw = json.dumps(payload).encode("utf-8")
    sys.stdout.buffer.write(struct.pack("<I", len(raw)))
    sys.stdout.buffer.write(raw)
    sys.stdout.buffer.flush()

def main():
    try:
        msg = read_message()
        if msg is None:
            return
        url = msg.get("url", "")
        parsed = urlparse(url)
        if parsed.scheme not in ("http", "https") or not parsed.netloc:
            raise ValueError("only valid HTTP/HTTPS URLs are accepted")
        base = Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local/share")) / "rdm"
        base.mkdir(parents=True, exist_ok=True)
        inbox = base / "browser-inbox.jsonl"
        record = {"url": url, "referrer": msg.get("referrer")}
        with inbox.open("a", encoding="utf-8") as f:
            f.write(json.dumps(record) + "\n")
        write_message({"ok": True})
    except Exception as exc:
        write_message({"ok": False, "error": str(exc)})

if __name__ == "__main__":
    main()
