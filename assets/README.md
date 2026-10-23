# Icons

Edit `tray-icon.svg`, the shared 24 × 24 Z symbol, then regenerate:

```sh
python3 scripts/generate-icons.py
```

Generation requires Python 3 with Pillow and `rsvg-convert` from librsvg. Generated files are checked in; building Zipover does not require these tools.

| Generated file | Use |
| --- | --- |
| `tray-icon.png` | 36 px transparent tray icon. macOS applies its template color; Windows displays the blue Z. |
| `zipover-symbolic.svg` | Linux symbolic tray icon. |
| `zipover.svg` | Desktop artwork: white Z on a cobalt rounded square. |
| `zipover.png` | 512 px Linux application icon and README logo. |
| `zipover.ico` | Windows executable icons, 16–256 px. |
| `zipover.icns` | macOS application icon, 16–1024 px including Retina sizes. |

Change desktop color, padding, and background in `scripts/generate-icons.py`. Edit the shared Z only in `tray-icon.svg`; the generator carries its geometry into every format.
