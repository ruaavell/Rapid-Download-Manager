#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
HOST_NAME="io.github.ruaavell.rdm"

# Locate the Rust native host binary.
HOST_BIN=""
if command -v rdm-native-host >/dev/null 2>&1; then
  HOST_BIN="$(command -v rdm-native-host)"
elif [[ -x "$ROOT/../target/release/rdm-native-host" ]]; then
  HOST_BIN="$ROOT/../target/release/rdm-native-host"
elif [[ -x "$ROOT/../target/debug/rdm-native-host" ]]; then
  HOST_BIN="$ROOT/../target/debug/rdm-native-host"
else
  echo "rdm-native-host binary not found. Build it first: cargo build -p rdm-native-host" >&2
  exit 1
fi

mkdir -p "$HOME/.local/bin" \
         "$HOME/.config/google-chrome/NativeMessagingHosts" \
         "$HOME/.config/chromium/NativeMessagingHosts" \
         "$HOME/.mozilla/native-messaging-hosts"

install -Dm755 "$HOST_BIN" "$HOME/.local/bin/rdm-native-host"
echo "Native host installed to $HOME/.local/bin/rdm-native-host"

# Generate a manifest pointing at the installed binary.
sed "s#/ABSOLUTE/PATH/TO/rdm-native-host#$HOME/.local/bin/rdm-native-host#g" \
  "$ROOT/native-host/manifests/chromium.example.json" > /tmp/rdm-chromium-host.json
echo "Manifest written to /tmp/rdm-chromium-host.json"
echo "Replace chrome-extension://REPLACE_WITH_EXTENSION_ID/ with your extension ID, then copy it to:"
echo "  $HOME/.config/chromium/NativeMessagingHosts/io.github.ruaavell.rdm.json"
echo "  $HOME/.config/google-chrome/NativeMessagingHosts/io.github.ruaavell.rdm.json"