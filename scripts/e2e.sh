#!/usr/bin/env bash
# End-to-end driver for aterm on a headless Linux box.
#
# Boots Xvfb + openbox, launches aterm, then exposes subcommands for typing,
# pressing key chords, clicking, hovering, and capturing window screenshots.
# Screenshots are written to /tmp/aterm-e2e/ with a numeric prefix so they
# sort by capture order; no automated diffing is performed.
#
# Usage:
#   scripts/e2e.sh setup                      # apt-get install deps (needs root)
#   scripts/e2e.sh start                      # boot Xvfb + openbox + aterm
#   scripts/e2e.sh type "echo hello"          # type literal text
#   scripts/e2e.sh key Return                 # press a key or chord (e.g. ctrl+l, super+t)
#   scripts/e2e.sh click [X Y]                # left-click at window-relative coords (default = center)
#   scripts/e2e.sh hover [-m MOD] [-l LBL] X Y # move mouse to window-relative X Y, optionally
#                                              # holding MOD; with -l also snap a screenshot
#                                              # while in that state, then release MOD.
#   scripts/e2e.sh snap LABEL                 # screenshot the aterm window
#   scripts/e2e.sh title                      # print the current window title
#   scripts/e2e.sh log [N]                    # tail aterm.log (default 30 lines)
#   scripts/e2e.sh state                      # print stored runtime state
#   scripts/e2e.sh stop                       # tear down
#   scripts/e2e.sh restart                    # stop + start

set -euo pipefail

E2E_DIR="${E2E_DIR:-/tmp/aterm-e2e}"
STATE="$E2E_DIR/state.env"
DISPLAY_NUM="${DISPLAY_NUM:-:99}"
SCREEN_SIZE="${SCREEN_SIZE:-1280x800x24}"
WIN_SIZE="${WIN_SIZE:-900x600}"
ATERM_BIN_DEFAULT="$(cd "$(dirname "$0")/.." && pwd)/target/release/aterm"
ATERM_BIN="${ATERM_BIN:-$ATERM_BIN_DEFAULT}"

mkdir -p "$E2E_DIR"

# Load existing state if any. Lines look like KEY=value.
load_state() {
    if [[ -f "$STATE" ]]; then
        # shellcheck disable=SC1090
        set -a; source "$STATE"; set +a
    fi
}

save_state() {
    {
        printf 'DISPLAY=%s\n' "$DISPLAY_NUM"
        printf 'WID=%s\n' "${WID:-}"
        printf 'WX=%s\n' "${WX:-0}"
        printf 'WY=%s\n' "${WY:-0}"
        printf 'XVFB_PID=%s\n' "${XVFB_PID:-}"
        printf 'OPENBOX_PID=%s\n' "${OPENBOX_PID:-}"
        printf 'ATERM_PID=%s\n' "${ATERM_PID:-}"
        printf 'SHOT_COUNTER=%s\n' "${SHOT_COUNTER:-0}"
    } > "$STATE"
}

require_running() {
    load_state
    export DISPLAY="$DISPLAY_NUM"
    if [[ -z "${ATERM_PID:-}" ]] || ! kill -0 "$ATERM_PID" 2>/dev/null; then
        echo "aterm is not running. Run: scripts/e2e.sh start" >&2
        exit 1
    fi
    # Refresh window origin every command so we tolerate WM repositioning.
    if [[ -n "${WID:-}" ]]; then
        local info
        info="$(xwininfo -id "$WID" 2>/dev/null || true)"
        if [[ -n "$info" ]]; then
            WX=$(awk '/Absolute upper-left X/ {print $4}' <<<"$info")
            WY=$(awk '/Absolute upper-left Y/ {print $4}' <<<"$info")
        fi
    fi
}

cmd_setup() {
    if ! command -v apt-get >/dev/null; then
        echo "non-apt distros: install xvfb, xdotool, imagemagick, openbox,"
        echo "  libxkbcommon-x11-0, mesa-vulkan-drivers manually" >&2
        exit 1
    fi
    apt-get update -y
    apt-get install -y --no-install-recommends \
        xvfb xdotool imagemagick openbox x11-utils \
        libxkbcommon-x11-0 libxkbcommon0 \
        mesa-vulkan-drivers libvulkan1
}

ensure_binary() {
    if [[ ! -x "$ATERM_BIN" ]]; then
        echo "building release binary at $ATERM_BIN" >&2
        ( cd "$(dirname "$ATERM_BIN")/../.." && cargo build --release )
    fi
}

cmd_start() {
    load_state
    if [[ -n "${ATERM_PID:-}" ]] && kill -0 "$ATERM_PID" 2>/dev/null; then
        echo "aterm already running (pid $ATERM_PID). Use 'stop' first." >&2
        exit 1
    fi

    ensure_binary

    # Xvfb. Reuse if a server is already up on this display.
    if ! DISPLAY="$DISPLAY_NUM" xdpyinfo >/dev/null 2>&1; then
        nohup Xvfb "$DISPLAY_NUM" -screen 0 "$SCREEN_SIZE" -ac \
            >"$E2E_DIR/xvfb.log" 2>&1 &
        XVFB_PID=$!
        sleep 1
    fi
    export DISPLAY="$DISPLAY_NUM"

    # Minimal WM so xdotool key events route to the focused window.
    if ! pgrep -x openbox >/dev/null; then
        nohup openbox >"$E2E_DIR/openbox.log" 2>&1 &
        OPENBOX_PID=$!
        sleep 1
    fi

    # aterm with verbose logs so we can grep for hover/url behavior.
    RUST_LOG="${RUST_LOG:-info,wgpu_core=warn,wgpu_hal=warn}" \
        nohup "$ATERM_BIN" >"$E2E_DIR/aterm.log" 2>&1 &
    ATERM_PID=$!

    # Wait for the window to map.
    local tries=0
    while ((tries < 50)); do
        WID="$(xdotool search --name aterm 2>/dev/null | head -1 || true)"
        [[ -n "$WID" ]] && break
        sleep 0.2
        ((tries++))
    done
    if [[ -z "${WID:-}" ]]; then
        echo "aterm window did not appear within 10s. See $E2E_DIR/aterm.log" >&2
        tail -n 30 "$E2E_DIR/aterm.log" >&2 || true
        exit 1
    fi

    xdotool windowactivate --sync "$WID"
    local info
    info="$(xwininfo -id "$WID")"
    WX=$(awk '/Absolute upper-left X/ {print $4}' <<<"$info")
    WY=$(awk '/Absolute upper-left Y/ {print $4}' <<<"$info")
    SHOT_COUNTER=0
    save_state
    echo "aterm pid=$ATERM_PID  window=$WID  origin=$WX,$WY  display=$DISPLAY_NUM"
}

cmd_stop() {
    load_state
    # If start reused already-running helpers, their PIDs aren't in state.
    # Look them up by name so we still tear everything down.
    : "${ATERM_PID:=$(pgrep -f "$ATERM_BIN" || true)}"
    : "${OPENBOX_PID:=$(pgrep -x openbox || true)}"
    : "${XVFB_PID:=$(pgrep -f "^Xvfb $DISPLAY_NUM" || true)}"
    for pid in $ATERM_PID $OPENBOX_PID $XVFB_PID; do
        kill "$pid" 2>/dev/null || true
    done
    sleep 0.3
    for pid in $ATERM_PID $OPENBOX_PID $XVFB_PID; do
        kill -9 "$pid" 2>/dev/null || true
    done
    rm -f "$STATE"
    echo "stopped"
}

cmd_type() {
    local text="$*"
    require_running
    xdotool windowactivate --sync "$WID"
    xdotool type --delay 30 -- "$text"
}

cmd_key() {
    require_running
    xdotool windowactivate --sync "$WID"
    xdotool key --clearmodifiers "$@"
}

cmd_click() {
    require_running
    local x="${1:-450}" y="${2:-300}"
    xdotool mousemove "$((WX + x))" "$((WY + y))"
    sleep 0.1
    xdotool click 1
}

cmd_hover() {
    local mod="" label="" sleep_after="0.5"
    while [[ "${1:-}" == -* ]]; do
        case "$1" in
            -m|--mod) mod="$2"; shift 2 ;;
            -l|--label) label="$2"; shift 2 ;;
            -s|--sleep) sleep_after="$2"; shift 2 ;;
            --) shift; break ;;
            *) echo "unknown flag: $1" >&2; exit 2 ;;
        esac
    done
    local x="${1:?hover X Y required}" y="${2:?hover X Y required}"
    require_running
    # Park the pointer well outside the window first so the move always
    # generates a fresh CursorEntered + CursorMoved sequence.
    xdotool mousemove 1 1
    sleep 0.1
    [[ -n "$mod" ]] && xdotool keydown "$mod"
    xdotool mousemove "$((WX + x))" "$((WY + y))"
    sleep "$sleep_after"
    if [[ -n "$label" ]]; then
        snap_now "$label"
    fi
    [[ -n "$mod" ]] && xdotool keyup "$mod"
    return 0
}

snap_now() {
    local label="$1"
    SHOT_COUNTER=$((${SHOT_COUNTER:-0} + 1))
    local n
    printf -v n '%03d' "$SHOT_COUNTER"
    local out="$E2E_DIR/${n}_${label}.png"
    import -window "$WID" "$out"
    save_state
    echo "$out"
}

cmd_snap() {
    require_running
    local label="${1:?label required}"
    snap_now "$label"
}

cmd_title() {
    require_running
    xdotool getwindowname "$WID"
}

cmd_log() {
    local n="${1:-30}"
    tail -n "$n" "$E2E_DIR/aterm.log" 2>/dev/null || echo "no aterm.log yet" >&2
}

cmd_state() {
    load_state
    [[ -f "$STATE" ]] && cat "$STATE" || echo "not running"
}

cmd_restart() { cmd_stop || true; cmd_start; }

main() {
    local sub="${1:-help}"; shift || true
    case "$sub" in
        setup) cmd_setup "$@" ;;
        start) cmd_start "$@" ;;
        stop)  cmd_stop "$@" ;;
        restart) cmd_restart "$@" ;;
        type)  cmd_type "$@" ;;
        key)   cmd_key "$@" ;;
        click) cmd_click "$@" ;;
        hover) cmd_hover "$@" ;;
        snap)  cmd_snap "$@" ;;
        title) cmd_title "$@" ;;
        log)   cmd_log "$@" ;;
        state) cmd_state "$@" ;;
        help|--help|-h|"") sed -n '2,40p' "$0" ;;
        *) echo "unknown subcommand: $sub" >&2; exit 2 ;;
    esac
}

main "$@"
