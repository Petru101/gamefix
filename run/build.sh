#!/bin/bash
# Static release build, so it runs on any host: target/x86_64-unknown-linux-gnu/release/gamefix
# The SDK has no static libm; std only needs its name.
set -o pipefail
host-spawn flatpak run --filesystem="$HOME/Projects/gaming" --command=sh org.gnome.Sdk//50 -c \
    'cd ~/Projects/gaming && mkdir -p stub && ar rc stub/libm.a &&
     PATH=/usr/lib/sdk/rust-stable/bin:$PATH RUSTFLAGS="-C target-feature=+crt-static -L native=$PWD/stub" \
     exec cargo --offline build --release --target x86_64-unknown-linux-gnu' \
    </dev/null | tr -d '\r'
