#!/usr/bin/env bash
# Build the release binary, put it where the manifest's commands expect it, link the plugin, and
# put `herdr-runner` on PATH.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

win_path() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -w "$1"
  else
    printf '%s\n' "$1"
  fi
}

ensure_windows_user_path() {
  local win
  win="$(win_path "$1")"
  HERDR_RUNNER_PATH_DIR="$win" powershell.exe -NoProfile -Command '
    $dir = $env:HERDR_RUNNER_PATH_DIR
    $user = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($null -eq $user) { $user = "" }
    $parts = @($user -split ";" | Where-Object { $_ -ne "" })
    if ($parts | Where-Object { $_.TrimEnd([char]92) -ieq $dir.TrimEnd([char]92) }) { exit 0 }
    $new = if ($user -eq "") { $dir } else { $user.TrimEnd(";") + ";" + $dir }
    [Environment]::SetEnvironmentVariable("Path", $new, "User")
  '
}

cargo build --release
mkdir -p bin "$HOME/.local/bin"

if [ "${OS-}" = Windows_NT ]; then
  cp -f target/release/herdr-runner.exe bin/herdr-runner.exe
  cp -f bin/herdr-runner.exe "$HOME/.local/bin/herdr-runner.exe"
  ensure_windows_user_path "$HOME/.local/bin"
  export PATH="$HOME/.local/bin:$PATH"
  herdr plugin link "$(win_path "$ROOT")"
else
  ln -sf ../target/release/herdr-runner bin/herdr-runner
  ln -sf "$ROOT/bin/herdr-runner" "$HOME/.local/bin/herdr-runner"
  herdr plugin link "$ROOT"
fi
