#!/usr/bin/env bash
# Run the test suite inside a Docker container so the same Xvfb + Mesa stack
# works identically on macOS and Linux hosts. Forwards any arguments to
# `cargo test` inside the container; the default runs only the integration
# tests.
#
# Examples:
#   ./scripts/test-docker.sh                    # run integration tests
#   ./scripts/test-docker.sh --                 # run ALL tests (unit + integration)
#   ./scripts/test-docker.sh --test integration boots_and_shows_prompt
#
# Failure screenshots from integration tests land in ./target/test-artifacts/
# on the host (bind-mounted to /artifacts inside the container).

set -euo pipefail

cd "$(dirname "$0")/.."

IMAGE="${IMAGE:-aterm-test}"
ARTIFACTS_DIR="${ARTIFACTS_DIR:-$PWD/target/test-artifacts}"

mkdir -p "$ARTIFACTS_DIR"

docker build -f scripts/Dockerfile.test -t "$IMAGE" .

# Named volumes cache cargo registry + target across runs so subsequent
# invocations are fast. Source is bind-mounted read/write.
docker run --rm \
    -e RUST_LOG="${RUST_LOG:-warn}" \
    -v "$PWD":/work \
    -v aterm-cargo-cache:/usr/local/cargo/registry \
    -v aterm-cargo-target:/work/target \
    -v "$ARTIFACTS_DIR":/artifacts \
    "$IMAGE" "$@"
