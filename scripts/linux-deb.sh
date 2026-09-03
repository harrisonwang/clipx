#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary_path=${1:?usage: scripts/linux-deb.sh GUI_BINARY OUTPUT_PATH VERSION}
output_path=${2:?usage: scripts/linux-deb.sh GUI_BINARY OUTPUT_PATH VERSION}
version=${3:?usage: scripts/linux-deb.sh GUI_BINARY OUTPUT_PATH VERSION}

if [ ! -x "$binary_path" ]; then
  printf '找不到 GUI 可执行文件：%s\n' "$binary_path" >&2
  exit 1
fi
command -v dpkg-deb >/dev/null 2>&1 || {
  printf '需要 dpkg-deb 才能构建 Ubuntu 安装包\n' >&2
  exit 1
}

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

mkdir -p \
  "$stage/DEBIAN" \
  "$stage/usr/bin" \
  "$stage/usr/share/applications" \
  "$stage/usr/share/icons/hicolor/scalable/apps"
cp "$binary_path" "$stage/usr/bin/clipx-gui"
cp "$repo_root/packaging/linux/clipx.desktop" "$stage/usr/share/applications/clipx.desktop"
cp "$repo_root/assets/tray-icon.svg" "$stage/usr/share/icons/hicolor/scalable/apps/clipx.svg"
sed "s/@VERSION@/$version/g" "$repo_root/packaging/linux/control" > "$stage/DEBIAN/control"

mkdir -p "$(dirname "$output_path")"
dpkg-deb --build --root-owner-group "$stage" "$output_path" >/dev/null
printf '已生成 %s\n' "$output_path"
