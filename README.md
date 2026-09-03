# clipx

`clipx` 是一个跨平台剪贴板工具，支持在 macOS、Windows 和 Linux 设备之间同步文本、图片、文件与目录。

## 安装

### Homebrew（macOS / Linux x86_64）

```bash
brew install harrisonwang/tap/clipx
```

macOS GUI 版本：

```bash
brew install --cask harrisonwang/tap/clipx-gui
```

### Scoop（Windows）

```powershell
scoop bucket add harrisonwang https://github.com/harrisonwang/scoop-bucket
scoop install clipx
```

Scoop 包同时提供 `clipx.exe` CLI、`clipx-gui.exe` GUI 入口和开始菜单快捷方式。

### 源码安装

```bash
cargo install --git https://github.com/harrisonwang/clipx
```

从源码编译桌面入口可运行 `cargo build --release && scripts/macos-app.sh`，然后双击生成的 `target/release/clipx.app`。`cargo run --bin clipx-gui` 会在当前终端运行，适合开发调试；`cargo run -- ...` 仍默认启动 CLI。

也可以从 [GitHub Releases](https://github.com/harrisonwang/clipx/releases) 下载预编译版本。

Linux 从源码编译桌面模式需要 GTK、AppIndicator 和 WebKitGTK：

```bash
sudo apt install libgtk-3-dev libappindicator3-dev libwebkit2gtk-4.1-dev
```

Ubuntu 22.04 的 GUI 安装包可在 GitHub Release 下载后直接安装：

```bash
sudo apt install ./clipx-gui_<version>_amd64.deb
```

## 使用

### 桌面模式

发布包中的 `clipx-gui` 是双击启动入口，不会打开命令行窗口。首次启动会让你选择角色并填写地址：

- 监听端默认使用 `0.0.0.0:45876`，可接受多个连接端。
- 连接端填写监听端地址，只连接一个监听端。

macOS 打开 DMG 后将 `clipx.app` 拖入 Applications；Windows 双击 `clipx-gui.exe` 或使用 MSI 安装包；Ubuntu 22.04 使用 `.deb` 安装包后即可从应用菜单启动。其它 Linux 发行版仍可将 `clipx-gui` 放入 `PATH` 后，把压缩包内的 `share/applications/clipx.desktop` 复制到 `~/.local/share/applications/`。

保存后，后续双击会直接进入托盘同步。配置文件位置为：

- macOS：`~/Library/Application Support/clipx/config`
- Windows：`%APPDATA%\clipx\config`
- Linux：`${XDG_CONFIG_HOME:-~/.config}/clipx/config`

CLI 入口仍然保留，适合脚本、无图形桌面环境和需要明确指定参数的场景。

在一台设备上监听：

```bash
clipx sync --listen 0.0.0.0:45876
```

在另一台设备上连接：

```bash
clipx sync --connect 192.168.1.10:45876
```

连接建立后即可双向同步剪贴板。一个监听端可以接受多个连接端；每个连接端只连接一个监听端。
`--listen` 和 `--connect` 不能同时使用。

加上 `--tray` 可在系统托盘运行。托盘菜单中的“打开设备面板”会根据运行角色显示对应信息：
监听端显示已连接设备和数量，连接端显示唯一连接目标和连接状态。

```bash
clipx sync --tray --listen 0.0.0.0:45876
```

连接端需要在启动时指定监听端地址：

```bash
clipx sync --tray --connect 192.168.1.10:45876
```

协议尚未启用身份认证和加密，请只连接可信局域网中的设备。

## 限制

- 同步没有配对、认证和加密，只应在可信局域网内使用。
- 两端必须使用相同的协议版本。
- Windows 同步进程必须从已登录桌面的终端启动，不能通过 SSH 或系统服务启动。
- Linux 需要可用的 X11 或 Wayland 剪贴板环境。
- Linux 托盘模式需要图形桌面会话；通过 SSH 或 TTY 启动时请使用不带 `--tray` 的同步命令。
- Linux 设备面板第一版使用 X11 和 WebKitGTK；Wayland 环境仍可使用 CLI 和托盘同步。
- 暂不支持符号链接、特殊文件和断点续传。

## 许可证

[MIT](LICENSE)
