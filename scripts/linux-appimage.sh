#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "$0")/.." && pwd)
binary_path=$(realpath "${1:?usage: scripts/linux-appimage.sh GUI_BINARY OUTPUT_PATH VERSION}")
output_path=$(realpath -m "${2:?usage: scripts/linux-appimage.sh GUI_BINARY OUTPUT_PATH VERSION}")
version=${3:?usage: scripts/linux-appimage.sh GUI_BINARY OUTPUT_PATH VERSION}
arch=$(uname -m)
case "$arch" in x86_64|aarch64) ;; *) echo "不支持的 AppImage 架构：$arch" >&2; exit 1 ;; esac

tools_dir="$repo_root/target/appimage-tools/$arch"
mkdir -p "$tools_dir" "$(dirname "$output_path")"
deploy="$tools_dir/linuxdeploy-$arch.AppImage"
plugin="$tools_dir/linuxdeploy-plugin-gtk.sh"
if [ ! -x "$deploy" ]; then
  curl -fL --retry 3 "https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-$arch.AppImage" -o "$deploy"
  chmod +x "$deploy"
fi
if [ ! -x "$plugin" ]; then
  curl -fL --retry 3 "https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/7a3fbc31a9e5075073ff8790f26effbac5f84453/linuxdeploy-plugin-gtk.sh" -o "$plugin"
  chmod +x "$plugin"
fi

# The tray library loads AppIndicator dynamically, so include it explicitly.
if pkg-config --exists ayatana-appindicator3-0.1; then
  indicator="$(pkg-config --variable=libdir ayatana-appindicator3-0.1)/libayatana-appindicator3.so.1"
else
  indicator="$(pkg-config --variable=libdir appindicator3-0.1)/libappindicator3.so.1"
fi

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
export PATH="$tools_dir:$PATH"
export APPIMAGE_EXTRACT_AND_RUN=1 DEPLOY_GTK_VERSION=3
export LDAI_OUTPUT="$output_path" LINUXDEPLOY_OUTPUT_VERSION="$version" LDAI_NO_APPSTREAM=1
"$deploy" --appdir "$stage/zipover.AppDir" \
  --executable "$binary_path" \
  --desktop-file "$repo_root/assets/linux/zipover.desktop" \
  --icon-file "$repo_root/assets/zipover.png" \
  --library "$indicator" --plugin gtk
# The GTK plugin forces X11; let GTK use the user's X11 or Wayland session.
sed -i '/^export GDK_BACKEND=x11 /d' "$stage/zipover.AppDir/apprun-hooks/linuxdeploy-plugin-gtk.sh"
"$deploy" --appdir "$stage/zipover.AppDir" --output appimage
test -x "$output_path"
printf '已生成 %s\n' "$output_path"
