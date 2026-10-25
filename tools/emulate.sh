#!/usr/bin/env bash
# Runs kiosk-ui in a desktop window the way it looks on the device, with the real kioskd
# behind it: the Wi-Fi list, the on-screen keyboard and the admin PIN all work. Only
# wpa_cli is replaced (tools/emulator/wpa_cli), so no network on this machine is touched.
#
#   tools/emulate.sh               800x480, the 5" touch panel
#   tools/emulate.sh 1280x720      the roomier layout
#
# Admin PIN: 1234. The password "wrongwrong" is always refused. Everything lives in a
# temporary directory that is removed on exit, so each run starts clean.
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
SIZE=${1:-800x480}
EMU=$(mktemp -d "${TMPDIR:-/tmp}/pikiosk-emu.XXXXXX")
KIOSKD= UI=
cleanup() { kill $UI $KIOSKD 2>/dev/null || true; rm -rf "$EMU"; }
trap cleanup EXIT
trap 'exit 130' INT TERM

cd "$HERE"
cargo build -q -p kioskd
cargo build -q -p kiosk-ui --features desktop

mkdir -p "$EMU/data" "$EMU/store" "$EMU/backlight" "$EMU/wpa"
echo 255 > "$EMU/backlight/max_brightness"
echo 255 > "$EMU/backlight/brightness"
echo 1234 > "$EMU/store.pin"
# the network the "device" starts on
mkdir -p "$EMU/wpa/net"
printf '%s' 'Home-5G' | od -An -tx1 -v | tr -d ' \n' > "$EMU/wpa/net/0.ssid"
echo supersecret > "$EMU/wpa/net/0.psk"
echo 0 > "$EMU/wpa/selected"
echo 1 > "$EMU/wpa/next"

TARGET=$(cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')

env KIOSKD_SOCK="$EMU/kioskd.sock" KIOSKD_STORE="$EMU/store" KIOSKD_INSTALL_DIRS="$EMU/store" \
    KIOSKD_WPA_CLI="$HERE/tools/emulator/wpa_cli" KIOSKD_WPA_SOCKDIR= KIOSKD_WIFI_TIMEOUT=6 \
    KIOSKD_PIN_FILE="$EMU/store.pin" KIOSKD_PIN_LOCKOUT=15 KIOSKD_HOME_GESTURE=no \
    EMU_WPA_DIR="$EMU/wpa" RUST_LOG=${RUST_LOG:-error} \
    "$TARGET/debug/kioskd" daemon &
KIOSKD=$!
for _ in $(seq 50); do [ -S "$EMU/kioskd.sock" ] && break; sleep 0.1; done

env KIOSKD_SOCK="$EMU/kioskd.sock" KIOSK_DATA="$EMU/data" KIOSK_BACKLIGHT="$EMU/backlight" \
    KIOSK_WINDOW="$SIZE" \
    "$TARGET/debug/kiosk-ui" --window &
UI=$!
wait $UI
