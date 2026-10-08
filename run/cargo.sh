#!/bin/bash
# Runs cargo from the GNOME SDK (rust-stable) on the host: run/cargo.sh check
set -o pipefail
host-spawn flatpak run --filesystem="$HOME/Projects/gaming" --command=sh org.gnome.Sdk//50 -c \
    'cd ~/Projects/gaming && PATH=/usr/lib/sdk/rust-stable/bin:$PATH exec cargo --offline "$@"' \
    sh "$@" </dev/null | tr -d '\r'
