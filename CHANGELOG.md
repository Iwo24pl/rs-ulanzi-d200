# Fork releases (Iwo24pl/rs-ulanzi-d200-windows)

## 0.0.3
* Plugin ID `com.iwo24pl.rs-ulanzi-d200`, binary `rs-ulanzi-d200`
* GitHub Actions builds for Windows and Linux, attached to releases automatically

## 0.0.2
* Send status-display frames only when content changes (fixes flicker)
* Renamed to `com.iwo24pl.rs-d200-windows` / `Ulanzi D200 Windows` (superseded by 0.0.3 naming)

## 0.0.1
* Initial Windows port: Win32 HID Report-ID prefix, `CodePathWin`, `pack.ps1`
* Re-register device on `plugin_ready` (fixes "no device connected" race)
* Config resolution relative to executable; Linux-only sysfs gating

# 0.6.5
* Saving status window state from previous sessions #14
* Remove remaining code from stand alone daemon
* Cleaning config.yaml
* Stability/Security updates on rust and libraries

# 0.6.4
* Change reverse domain to com.glmagalhaes.ulanzi.d200, added the supported device that was missing
* This name will be final for automatic updates in the future
* Launched on OpenDeck's OpenAction Marketplace #10

# 0.6.3
* Support for GPU load in status window #8
* Better organization in code
* Change in plugin namming, internal and external #11

# 0.6.2
 * Mapped all the possible screens that the status window has
 * Added an action to witch cycle what's shown on status window

# 0.6.1
 * Improved algorithm circumventing the hardware bug adding a lot more entropy
 * Removed blinking caused by the device recieving too many packets too fast
 * Reduced the number of packages sent to the device

# 0.6.0
 * Improved on how to circumvent the hardware bug
 * Added an icon to the packaged version of the plug-in
 * Updated info to show that It also works with Ulanzi D200(H)
 * Added a shell script to package the plug-in correctly

# 0.5.0
 * Circumvented a known bug in the hardware crash depending on the values in certain zip positions
 * Reduced racing conditions when sending data to device