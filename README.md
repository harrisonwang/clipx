# clipx

`clipx` 是一个跨平台剪贴板工具，支持在 macOS、Windows 和 Linux 设备之间同步文本、图片、文件与目录。

## 安装

### Homebrew（macOS / Linux x86_64）

```bash
brew install harrisonwang/tap/clipx
```

### Scoop（Windows）

```powershell
scoop bucket add harrisonwang https://github.com/harrisonwang/scoop-bucket
scoop install clipx
```

### 源码安装

```bash
cargo install --git https://github.com/harrisonwang/clipx
```

也可以从 [GitHub Releases](https://github.com/harrisonwang/clipx/releases) 下载预编译版本。

## 使用

在一台设备上监听：

```bash
clipx sync --listen 0.0.0.0:45876
```

在另一台设备上连接：

```bash
clipx sync --connect 192.168.1.10:45876
```

连接建立后即可双向同步剪贴板。`--connect` 可以重复指定，以连接多台设备。

## 限制

- 同步没有配对、认证和加密，只应在可信局域网内使用。
- 两端必须使用相同的协议版本。
- Windows 同步进程必须从已登录桌面的终端启动，不能通过 SSH 或系统服务启动。
- Linux 需要可用的 X11 或 Wayland 剪贴板环境。
- 暂不支持符号链接、特殊文件和断点续传。

## 许可证

[MIT](LICENSE)
