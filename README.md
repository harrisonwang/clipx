# clipx

跨平台剪贴板同步工具，支持 macOS、Windows 和 Ubuntu 22.04。可同步文本、图片、文件与目录。

## 安装

### Desktop

| 平台 | 获取方式 |
| --- | --- |
| macOS | 从 [GitHub Releases](https://github.com/harrisonwang/clipx/releases) 下载 DMG |
| Windows | 从 [GitHub Releases](https://github.com/harrisonwang/clipx/releases) 下载 MSI |
| Ubuntu 22.04 | 从 [GitHub Releases](https://github.com/harrisonwang/clipx/releases) 下载 DEB |

Desktop 首次启动时选择连接方式并填写地址，启动后会常驻系统托盘：

- 监听端：监听 `0.0.0.0:45876`，可接受多个连接端。
- 连接端：填写监听端地址，只连接一个监听端。

配置文件位置：

- macOS：`~/Library/Application Support/clipx/config`
- Windows：`%APPDATA%\\clipx\\config`
- Ubuntu：`~/.config/clipx/config`

### CLI

#### macOS / Ubuntu

```bash
brew install harrisonwang/tap/clipx

# 安装 GUI 版本
brew install --cask harrisonwang/tap/clipx-gui
```

#### Windows

```bash
scoop bucket add harrisonwang https://github.com/harrisonwang/scoop-bucket
scoop install clipx
```

## 使用

### CLI

在一台设备上监听：

```bash
clipx sync --listen 0.0.0.0:45876
```

在另一台设备上连接：

```bash
clipx sync --connect 192.168.1.10:45876
```

## 开发

### Desktop

先安装 Rust。Ubuntu 22.04 还需要安装 GUI 依赖：

```bash
sudo apt install libgtk-3-dev libappindicator3-dev libwebkit2gtk-4.1-dev
```

macOS：

```bash
cargo build --release --bin clipx-gui
scripts/macos-app.sh
open target/release/clipx.app
```

Windows：

```powershell
cargo build --release --bin clipx-gui
```

然后双击 `target\\release\\clipx-gui.exe`。

Ubuntu：

```bash
cargo build --release --bin clipx-gui
./target/release/clipx-gui
```

Ubuntu GUI 需要在图形桌面会话中启动。

### CLI

```bash
cargo run -- sync --listen 0.0.0.0:45876
cargo run -- sync --connect 192.168.1.10:45876
```

运行测试：

```bash
cargo test --locked --workspace --all-targets
```

## 限制

- 仅适用于可信局域网，当前协议未提供身份认证和加密。
- 两端必须使用相同的协议版本。
- Linux 需要可用的 X11 或 Wayland 剪贴板环境。
- 不支持符号链接、特殊文件和断点续传。

## 许可证

[MIT](LICENSE)
