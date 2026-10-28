#!/usr/bin/env bash
# Build the release binary, put it where the manifest's commands expect it, and link the plugin.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

cargo build --release
mkdir -p bin
ln -sf ../target/release/herdr-runner bin/herdr-runner
herdr plugin link "$ROOT"
