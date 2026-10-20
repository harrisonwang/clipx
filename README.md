# clipx

在局域网内同步文本、图片、文件和目录。配对一次，之后自动连接并加密传输，无需账号或服务器。

Rust 同步核心，提供 CLI 和原生托盘应用。桌面版使用 macOS AppKit、Windows Win32 和 Linux GTK。

> v0.7.0 引入原生桌面界面与加密配对。从旧版升级时，两端需一起更新并重新配对。

## 安装

下载地址：[GitHub Releases](https://github.com/harrisonwang/clipx/releases)。

| 平台 | 桌面版 | CLI |
| --- | --- | --- |
| macOS | DMG | tar.gz |
| Windows | 便携 EXE | EXE |
| Linux | AppImage | tar.gz |

macOS 可通过 Homebrew 分别安装：

```sh
brew install harrisonwang/tap/clipx             # CLI
brew install --cask harrisonwang/tap/clipx-gui  # 桌面版
```

Windows 可通过 Scoop 安装，包含 CLI、桌面版及开始菜单快捷方式：

```powershell
scoop bucket add harrisonwang https://github.com/harrisonwang/scoop-bucket
scoop install clipx
```

## 使用

**桌面版**：启动后常驻菜单栏或系统托盘。在第一台设备生成配对码，在第二台输入这 6 位数字，即可开始同步。托盘菜单可查看连接状态、添加或移除设备。

**CLI**：在两台设备上分别运行：

```sh
# 设备 A：首次运行生成配对码，并等待连接
clipx

# 设备 B：输入 A 显示的配对码，保留开头的 0
clipx --pair 001234
```

配对码两分钟内有效。配对成功后，两端自动进入同步并保持运行；以后两端直接运行 `clipx` 即可。

```sh
clipx --tray                  # 启动已安装的桌面版
clipx devices                 # 查看已配对设备
clipx forget <设备ID>         # 解除配对，先退出本机同步进程
clipx copy <文件或目录>...    # 将文件或目录放入本机剪贴板
clipx --help                  # 查看更多选项
```

两端须处于可互通的 IPv4 局域网。自动发现使用 UDP 5353，同步默认使用 TCP 45876；macOS 需允许局域网访问。Linux 配对与同步需在可访问 X11 或 Wayland 剪贴板的会话中运行。

收到的内容会替换本机剪贴板，不保留历史记录。

## 开发

需要 Rust stable。Ubuntu 构建桌面版需先安装系统依赖：

```sh
sudo apt install libgtk-3-dev libayatana-appindicator3-dev
```

```sh
cargo build --locked --release --workspace
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --all-targets
```

构建产物为 `target/release/clipx` 和 `clipx-gui`，Windows 下带 `.exe` 后缀。macOS 运行 `scripts/macos-dmg.sh` 生成 `.app` 与 DMG。

`crates/clipx-core` 负责配对与同步，`crates/clipx-cli` 和 `crates/clipx-desktop` 分别提供命令行与原生界面。

## 许可证

[MIT](LICENSE)
