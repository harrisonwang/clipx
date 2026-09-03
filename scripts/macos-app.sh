#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary_path=${1:-"$repo_root/target/release/clipx-gui"}
app_path=${2:-"$repo_root/target/release/clipx.app"}
version=${3:-$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$repo_root/Cargo.toml" | head -n 1)}

if [ ! -x "$binary_path" ]; then
  printf '找不到 GUI 可执行文件：%s\n请先运行 cargo build --release。\n' "$binary_path" >&2
  exit 1
fi

rm -rf "$app_path"
mkdir -p "$app_path/Contents/MacOS" "$app_path/Contents/Resources"
cp "$binary_path" "$app_path/Contents/MacOS/clipx-gui"
sed "s/@VERSION@/$version/g" "$repo_root/packaging/macos/Info.plist" \
  > "$app_path/Contents/Info.plist"

printf '已生成 %s\n' "$app_path"
