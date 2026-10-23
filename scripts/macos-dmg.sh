#!/bin/sh
set -eu

# Usage: scripts/macos-dmg.sh [GUI_BINARY] [DMG_PATH] [VERSION]
# Keep the .app alongside the DMG for local use and archive packaging.
repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary_path=${1:-"$repo_root/target/release/zipover-gui"}
output_path=${2:-"$repo_root/target/release/zipover.dmg"}
app_path="$(dirname "$output_path")/Zipover.app"
version=${3:-$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$repo_root/Cargo.toml" | head -n 1)}

if [ ! -x "$binary_path" ]; then
  printf '找不到 GUI 可执行文件：%s\n请先运行 cargo build --release -p zipover-desktop。\n' "$binary_path" >&2
  exit 1
fi

rm -rf "$app_path"
mkdir -p "$app_path/Contents/MacOS" "$app_path/Contents/Resources"
cp "$binary_path" "$app_path/Contents/MacOS/zipover-gui"
cp "$repo_root/assets/zipover.icns" "$app_path/Contents/Resources/zipover.icns"
sed "s/@VERSION@/$version/g" "$repo_root/assets/macos/Info.plist" \
  > "$app_path/Contents/Info.plist"
codesign --force --sign "${CODESIGN_IDENTITY:--}" "$app_path"

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

cp -R "$app_path" "$stage/Zipover.app"
ln -s /Applications "$stage/Applications"
hdiutil create \
  -volname Zipover \
  -srcfolder "$stage" \
  -ov \
  -format UDZO \
  "$output_path" \
  >/dev/null

printf '已生成 %s\n' "$app_path" "$output_path"
