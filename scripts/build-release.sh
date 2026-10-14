#!/bin/bash
# release 门禁单命令（r39/r43 门禁顺序两次复发的根治）：
# vite build → touch static_assets.rs（强制 rust-embed 重嵌）→ cargo build --release。
# set -e 任一步失败即中断；结尾输出 dist sha 与产物 mtime 供提测核对。
set -e
cd "$(dirname "$0")/../web"
npm run build
cd ..
touch crates/server/src/static_assets.rs
cargo build --release
echo "── release 产物核对 ──"
echo "dist index.html sha256(12): $(shasum -a 256 web/dist/index.html | cut -c1-12)"
echo "dist mtime: $(stat -f '%Sm' web/dist/index.html)"
echo "release bin mtime: $(stat -f '%Sm' target/release/server)"
