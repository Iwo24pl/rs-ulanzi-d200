![OpenDeck Ulanzi D200 Driver Logo](src/assets/icon.png)

# OpenDeck Ulanzi D200 Driver (Unofficial)

> **Fork with Windows support.** This is a community fork maintained at
> [`Iwo24pl/rs-ulanzi-d200-windows`](https://github.com/Iwo24pl/rs-ulanzi-d200-windows),
> based on [`glmagalhaes/rs-ulanzi-d200`](https://github.com/glmagalhaes/rs-ulanzi-d200)
> (upstream source on
> [GitLab](https://gitlab.com/glmagalhaes.mail/rs-ulanzi-d-200-linux)).
> Please open issues on this fork, not upstream.

An unofficial plugin for [OpenDeck](https://github.com/nekename/OpenDeck) that adds support for the Ulanzi D200 and D200H devices.

> **Recommendation:** For best compatibility, update your device firmware using the official **Ulanzi Studio** (available for macOS and Windows).

---

## Supported Devices

- Ulanzi D200 (USB ID `2207:0019`)
- Ulanzi D200H (USB ID `2207:0019`)

The D200H is identical to the D200 but includes two additional USB hubs (Genesys Logic, Inc., `05e3:0610`).

---

## Platform Support

| Platform | Status |
|----------|--------|
| Windows  | ✅ Supported by this fork |
| Linux    | ✅ Supported |
| macOS    | ❌ Not supported |

---

## Installation

1. Download the Windows or Linux build from the [releases page](https://github.com/Iwo24pl/rs-ulanzi-d200-windows/releases).
2. In OpenDeck, go to **Plugins → Install from file** and select the archive.
3. The plugin will appear in your plugin list.

---

## Actions

### Screen Switch

Cycles the built‑in status display of the wide button through three modes:

- **Blank** – it will show empty or the icon of your choice
- **Clock** – show current time
- **PC stats** – displays CPU, RAM, and GPU load

This action does **not** affect the button’s ability to send key presses. It only changes the visual information shown on the device’s screen.

---

## Building from Source

Requirements: Rust, Cargo, and standard build tools (e.g., `git`, `make`).

Linux: the `pack.sh` script compiles the plugin and packages it as a `.zip` file.
Windows: `pack.ps1` is the PowerShell equivalent.

```sh
# Release build (optimized)
sh pack.sh release
```

```powershell
# Release build (optimized)
powershell -ExecutionPolicy Bypass -File ./pack.ps1 release
```

Every version tag (`v*`) is also built for Windows and Linux by GitHub Actions,
and both zips are attached to the GitHub release automatically.

---

## Known issues

### Stretched Icon on Wide button

OpenDeck currently only supports a grid of square buttons (e.g., 5×3). The Ulanzi D200 has a wide button that spans two columns. Because OpenDeck treats every cell as an independent square, the icon assigned to that button appears stretched horizontally.

### Extra empty button

Since OpenDeck’s grid is always rectangular, the plugin must define a fixed number of rows and columns. On the D200, this creates a “ghost” button in the bottom‑right position (row 3, column 5) that does not exist on the physical device. This button is non‑functional and can be ignored, or you can just store an spare action there ¯\\_(ツ)_/¯.


### Wide button not working

If the wide button does not register key presses correctly, the device firmware might be outdated.
Update the firmware using **Ulanzi Studio** (available for macOS and Windows) – this is the only official method provided by Ulanzi. After updating, the button should function correctly with the plugin.

---

## Fork differences from upstream

- Windows support (`CodePathWin`, Win32 HID Report-ID handling, `pack.ps1`)
- Device re-registration on `plugin_ready` (fixes "no device connected" race)
- Status display frames are sent only when content changes (fixes flicker)
- GitHub Actions builds for Windows and Linux on every version tag

## Contributing

Contributions are welcome! Please open an issue first to discuss major changes.

## Support the Project

If you find this plugin useful, consider supporting its development with a donation via Pix.

![Pix QR Code](assets/pix-qr-code.png)

**Pix key:** `glmagalhaes@hotmail.com` 

## License
This project is licensed under the GNU Affero General Public License v3.0 – see the LICENSE file for details.