# clipx

`clipx` 是一个极简的命令行剪贴板工具，支持把文件、目录、文本和图片在同一局域网的 Mac、Windows、Linux 设备之间同步。

## 直接复制文件或目录

```bash
clipx image.png
clipx my-folder
```

命令成功后，可以在文件管理器中使用系统正常的粘贴快捷键：macOS 使用 `Cmd+V`，Windows/Linux 使用 `Ctrl+V`。文件路径会写入系统文件剪贴板，源文件不会被修改。

## 编译安装

```bash
cargo build --release
cargo install --path .
```

安装后可直接使用 `clipx` 命令。Windows 上生成的文件是 `clipx.exe`。

## 局域网剪贴板同步

在 Mac 上启动监听端：

```bash
clipx sync --listen 0.0.0.0:45876
```

在另一台设备上连接 Mac：

```bash
clipx sync --peer <Mac局域网IP>:45876
```

一个 TCP 连接同时支持双向同步。进程保持运行后，在任意一端复制内容，另一端就可以使用系统正常的粘贴操作。

Windows 必须在已登录桌面的 PowerShell 或 Windows Terminal 中运行同步进程：

```powershell
cargo install --path . --force
& "$env:USERPROFILE\.cargo\bin\clipx.exe" sync --peer <Mac局域网IP>:45876
```

不要通过 SSH 或 Windows 服务启动同步进程。它们通常运行在 Session 0，无法访问当前桌面会话的系统剪贴板。SSH 可以用于编译，但不能用于启动同步进程。

## 传输行为

- 文本和图片使用单帧传输，单条消息最大约 4 MiB。
- 文件和目录使用 1 MiB 分块传输，单次复制最多 32 GiB、100,000 个文件或目录。
- 每个文件接收完成后进行 SHA-256 校验，校验成功才从临时文件原子重命名为最终文件。
- 目录按相对路径重建，所有文件完成后才写入系统文件剪贴板。
- 临时文件位于系统临时目录下的 `clipx-sync`，启动时会清理超过 7 天的旧目录。
- 暂不支持符号链接和特殊文件，也不支持断点续传。

## 安全说明

当前同步模式没有配对、认证和加密，只适合可信的私有局域网。不要把监听端口暴露到公网。

## 平台说明

程序通过 `arboard` 使用 macOS、Windows 和 Linux 的原生剪贴板。Linux 需要可用的 X11 或 Wayland 剪贴板环境。部分 X11 环境需要剪贴板管理器，才能让短命令行进程退出后仍保留剪贴板内容。
