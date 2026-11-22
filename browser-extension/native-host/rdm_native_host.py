#!/usr/bin/env python3
"""Minimal Native Messaging host.

Receives download requests from the browser extension and appends them to an
inbox JSONL file that the desktop app polls. The file name the browser already
resolved and the cookies for the link are forwarded too: without them servers
such as Google Drive answer with a web page and the saved file is misnamed and
corrupt. Cookies only live in this owner-only file, which the desktop app
deletes once it has read it. No shell command is executed and no URL is
interpolated into a command.
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
        if msg.get("ping"):
            write_message({"ok": True})
            return
        url = msg.get("url", "")
        action = msg.get("action", "add")
        parsed = urlparse(url)
        if parsed.scheme not in ("http", "https") or not parsed.netloc:
            raise ValueError("only valid HTTP/HTTPS URLs are accepted")
        base = Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local/share")) / "rdm"
        base.mkdir(parents=True, exist_ok=True)
        inbox = base / "browser-inbox.jsonl"

        def optional_text(key):
            value = msg.get(key)
            return value.strip() if isinstance(value, str) and value.strip() else None

        record = {
            "url": url,
            "referrer": optional_text("referrer"),
            "filename": optional_text("filename"),
            "cookies": optional_text("cookies"),
        }
        # Cookies are credentials: keep the inbox readable by the owner only.
        inbox.touch(mode=0o600, exist_ok=True)
        os.chmod(inbox, 0o600)
        with inbox.open("a", encoding="utf-8") as f:
            f.write(json.dumps(record) + "\n")
        write_message({"ok": True, "action": action})
    except Exception as exc:
        write_message({"ok": False, "error": str(exc)})

if __name__ == "__main__":
    main()
