#!/usr/bin/env bash
# Ad-hoc harness for poking at aterm by hand. Boots Xvfb + openbox + aterm
# with the debug IPC socket enabled, then lets you send JSON requests from
# the shell. Anything you'd want to assert on belongs in tests/integration.rs.
#
#   scripts/e2e.sh setup                  # apt-get install deps (needs root)
#   scripts/e2e.sh start                  # boot everything; build aterm if needed
#   scripts/e2e.sh stop                   # tear everything down
#   scripts/e2e.sh ipc <cmd> [k=v ...]    # one IPC request
#   scripts/e2e.sh ipc raw '<json>'       # one IPC request, payload verbatim

set -euo pipefail

E2E_DIR="${E2E_DIR:-/tmp/aterm-e2e}"
DISPLAY_NUM="${DISPLAY_NUM:-:99}"
DEBUG_SOCK="$E2E_DIR/aterm.sock"
ATERM_BIN="$(cd "$(dirname "$0")/.." && pwd)/target/release/aterm"

mkdir -p "$E2E_DIR"

cmd_setup() {
    apt-get update -y
    apt-get install -y --no-install-recommends \
        xvfb imagemagick openbox x11-utils \
        libxkbcommon-x11-0 libxkbcommon0 \
        mesa-vulkan-drivers libvulkan1
}

cmd_start() {
    [[ -x "$ATERM_BIN" ]] || ( cd "$(dirname "$ATERM_BIN")/../.." && cargo build --release )

    DISPLAY="$DISPLAY_NUM" xdpyinfo >/dev/null 2>&1 || {
        nohup Xvfb "$DISPLAY_NUM" -screen 0 1280x800x24 -ac \
            >"$E2E_DIR/xvfb.log" 2>&1 &
        sleep 1
    }
    pgrep -x openbox >/dev/null || {
        DISPLAY="$DISPLAY_NUM" nohup openbox >"$E2E_DIR/openbox.log" 2>&1 &
        sleep 1
    }

    rm -f "$DEBUG_SOCK"
    DISPLAY="$DISPLAY_NUM" \
    RUST_LOG="${RUST_LOG:-info,wgpu_core=warn,wgpu_hal=warn}" \
    ATERM_DEBUG_SOCK="$DEBUG_SOCK" \
        nohup "$ATERM_BIN" >"$E2E_DIR/aterm.log" 2>&1 &

    # Wait for the debug socket — the canonical "aterm is ready" signal.
    local i
    for i in $(seq 50); do
        [[ -S "$DEBUG_SOCK" ]] && break
        sleep 0.2
    done
    if [[ ! -S "$DEBUG_SOCK" ]]; then
        echo "aterm did not come up; see $E2E_DIR/aterm.log" >&2
        tail -n 30 "$E2E_DIR/aterm.log" >&2 || true
        exit 1
    fi
    echo "ready: sock=$DEBUG_SOCK display=$DISPLAY_NUM log=$E2E_DIR/aterm.log"
}

cmd_stop() {
    pkill -f "$ATERM_BIN" 2>/dev/null || true
    pkill -x openbox 2>/dev/null || true
    pkill -f "^Xvfb $DISPLAY_NUM" 2>/dev/null || true
    rm -f "$DEBUG_SOCK"
}

# Send one line-delimited JSON request to the debug socket and print the
# response. Uses Python so we don't depend on any particular nc flavor.
cmd_ipc() {
    [[ -S "$DEBUG_SOCK" ]] || { echo "no socket at $DEBUG_SOCK (run 'start' first)" >&2; exit 1; }
    [[ $# -ge 1 ]] || { echo "usage: e2e.sh ipc <cmd> [k=v ...]   or: ipc raw '<json>'" >&2; exit 2; }

    local payload
    if [[ "$1" == "raw" ]]; then
        payload="$2"
    else
        local cmd="$1"; shift
        payload="{\"cmd\":\"$cmd\""
        for kv in "$@"; do
            local k="${kv%%=*}" v="${kv#*=}"
            if [[ "$v" =~ ^(true|false|null|-?[0-9]+(\.[0-9]+)?|\[.*\]|\{.*\})$ ]]; then
                payload+=",\"$k\":$v"
            else
                payload+=",\"$k\":\"$v\""
            fi
        done
        payload+="}"
    fi

    python3 - "$DEBUG_SOCK" "$payload" <<'PY'
import socket, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.settimeout(15)
s.connect(sys.argv[1])
s.sendall(sys.argv[2].encode() + b"\n")
buf = b""
while not buf.endswith(b"\n"):
    chunk = s.recv(4096)
    if not chunk:
        break
    buf += chunk
print(buf.decode().rstrip("\n"))
PY
}

case "${1:-help}" in
    setup) shift; cmd_setup "$@" ;;
    start) shift; cmd_start "$@" ;;
    stop)  shift; cmd_stop "$@" ;;
    ipc)   shift; cmd_ipc "$@" ;;
    *) sed -n '2,9p' "$0"; exit 0 ;;
esac
