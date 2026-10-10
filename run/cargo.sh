#!/bin/bash
# Runs cargo from the GNOME SDK (rust-stable) on the host: run/cargo.sh check
set -o pipefail
# The checkout this script is in (a worktree too).
dir=$(cd "$(dirname "$0")/.." && pwd)
host-spawn flatpak run --filesystem="$dir" --command=sh org.gnome.Sdk//50 -c \
    'cd "$0" && PATH=/usr/lib/sdk/rust-stable/bin:$PATH exec cargo --offline "$@"' \
    "$dir" "$@" </dev/null | tr -d '\r'
