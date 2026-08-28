# copy

Copy a file into the system clipboard from the command line.

## Usage

```text
copy image.png
```

After the command succeeds, open a Finder folder and press `Cmd+V` to copy the PNG file into that folder. On Windows and Linux, paste into a file manager with `Ctrl+V`.

The command copies a file URL, which is the format expected by Finder and other file managers. Applications such as WeChat and rich-text editors may resolve an image file URL and insert the image contents. The source file is not modified.

## Build and install

```bash
cargo build --release
cargo install --path .
```

This installs the executable as `copy`.

On PowerShell, `copy` is commonly an alias for `Copy-Item`; invoke `copy.exe image.png` explicitly or define your own shell function.

## Platform notes

The `arboard` backend uses the native clipboard on macOS, Windows, and Linux. Linux desktop sessions must expose a working X11 or Wayland clipboard. On some X11 setups, clipboard contents can disappear when the owning process exits unless a clipboard manager is running.
