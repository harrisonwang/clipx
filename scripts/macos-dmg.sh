#!/bin/sh
set -eu

app_path=${1:?usage: scripts/macos-dmg.sh APP_PATH OUTPUT_PATH [VOLUME_NAME]}
output_path=${2:?usage: scripts/macos-dmg.sh APP_PATH OUTPUT_PATH [VOLUME_NAME]}
volume_name=${3:-clipx}

if [ ! -d "$app_path" ]; then
  printf '找不到 macOS app bundle：%s\n' "$app_path" >&2
  exit 1
fi

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

mkdir -p "$(dirname "$output_path")"
cp -R "$app_path" "$stage/clipx.app"
ln -s /Applications "$stage/Applications"
rm -f "$output_path"
hdiutil create \
  -volname "$volume_name" \
  -srcfolder "$stage" \
  -ov \
  -format UDZO \
  "$output_path" \
  >/dev/null

printf '已生成 %s\n' "$output_path"
