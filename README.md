<p align="center">
  <img src="assets/zipover.png" width="96" alt="Zipover">
</p>

<h1 align="center">Zipover</h1>

<p align="center">
  <strong>Copy here. Paste there.</strong><br>
  A private clipboard for your computers.
</p>

<p align="center">
  macOS · Windows · Linux<br>
  <a href="https://github.com/harrisonwang/zipover/releases/latest">Download</a> ·
  <a href="README.CN.md">简体中文</a>
</p>

Copy text, images, files, or folders on one computer and paste them on another. Zipover connects your computers directly over your local network, with encrypted transfers and no account or cloud service required.

## Install

### macOS

Using [Homebrew](https://brew.sh):

```sh
brew install --cask harrisonwang/tap/zipover-gui
```

### Windows

Using [Scoop](https://scoop.sh):

```powershell
scoop bucket add harrisonwang https://github.com/harrisonwang/scoop-bucket
scoop install zipover
```

### Linux

```sh
curl -fsSL https://raw.githubusercontent.com/harrisonwang/zipover/main/scripts/install-linux.sh | bash
```

The installer supports x86_64 Linux. On GNOME, enable an AppIndicator extension to show the tray icon.

## Pair your computers

1. Open Zipover on two computers connected to the same local network.
2. Use the menu bar or tray icon to open the pairing window. Display a six-digit code on one computer and enter it on the other.
3. Leave Zipover running on both computers, then copy and paste as usual.

You only need to pair once. Add more computers the same way.

## Command line

On macOS and Linux, install the CLI with Homebrew:

```sh
brew install harrisonwang/tap/zipover
```

The Windows Scoop package includes the CLI.

```sh
zipover                    # Start syncing; first run shows a pairing code
zipover --pair 123456       # Enter the code shown on another computer
zipover devices            # List paired computers
zipover copy ./file.txt     # Copy a file or folder to the clipboard
zipover --help              # Show all options
```

Run one instance per computer, using either the desktop app or the CLI. On Linux, the CLI needs an active desktop session to access the clipboard.

## Build from source

Use the latest stable Rust toolchain. On Ubuntu, install `libgtk-3-dev` and `libayatana-appindicator3-dev` first.

```sh
cargo build --locked --release --workspace
cargo test --locked --workspace --all-targets
```

[MIT License](LICENSE)
