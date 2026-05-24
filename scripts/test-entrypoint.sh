#!/usr/bin/env bash
# Bring up Xvfb + openbox inside the container, then exec cargo test with the
# arguments the caller passed in (default: just the integration suite).
# Failure-screenshot output is written under $ATERM_TEST_ARTIFACTS, which the
# host wrapper bind-mounts so the PNGs survive container teardown.

set -euo pipefail

mkdir -p "$ATERM_TEST_ARTIFACTS"

Xvfb "$DISPLAY" -screen 0 1280x800x24 -ac >/tmp/xvfb.log 2>&1 &
XVFB_PID=$!

# Poll until the X server is up; otherwise wgpu will see "no display" and the
# very first test will crash before its screenshot helper can run.
for _ in $(seq 1 50); do
    if xdpyinfo >/dev/null 2>&1; then break; fi
    sleep 0.1
done
if ! xdpyinfo >/dev/null 2>&1; then
    echo "Xvfb did not come up; aborting test run" >&2
    cat /tmp/xvfb.log >&2 || true
    exit 1
fi

openbox >/tmp/openbox.log 2>&1 &

cleanup() {
    kill "$XVFB_PID" 2>/dev/null || true
}
trap cleanup EXIT

exec cargo test "$@"
