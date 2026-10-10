#!/bin/bash
# Static release build, so it runs on any host: target/x86_64-unknown-linux-gnu/release/gamefix
# The SDK has no static libm; std only needs its name.
set -o pipefail
# The checkout this script is in (a worktree too).
dir=$(cd "$(dirname "$0")/.." && pwd)
host-spawn flatpak run --filesystem="$dir" --command=sh org.gnome.Sdk//50 -c \
    'cd "$0" && mkdir -p stub && ar rc stub/libm.a &&
     PATH=/usr/lib/sdk/rust-stable/bin:$PATH RUSTFLAGS="-C target-feature=+crt-static -L native=$PWD/stub" \
     exec cargo --offline build --release --target x86_64-unknown-linux-gnu' \
    "$dir" </dev/null | tr -d '\r'
