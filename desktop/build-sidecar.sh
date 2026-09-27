#!/bin/sh
# 构建 release server 并按 tauri externalBin 约定（<name>-<target-triple>）拷入 src-tauri/binaries/
set -e
cd "$(dirname "$0")/.."
cargo build --release -p server
triple=$(rustc -vV | awk '/host:/{print $2}')
dest="desktop/src-tauri/binaries/server-$triple"
mkdir -p "$(dirname "$dest")"
cp target/release/server "$dest"
echo "sidecar → $dest"
