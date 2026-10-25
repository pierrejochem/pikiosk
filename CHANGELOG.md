# Changelog

## Unreleased

**Development**
- `tools/emulate.sh` runs kiosk-ui and the real `kioskd` in a desktop window, at 800×480 (the 5" panel) or any size (`tools/emulate.sh 1280x720`). Only `wpa_cli` is replaced, by `tools/emulator/wpa_cli` with made-up networks. Admin PIN `1234`; the password `wrongwrong` is refused.
- `KIOSK_WINDOW=WxH` sets the size of the desktop window, and `KIOSKD_WIFI_TIMEOUT` the seconds `kioskd` waits for a Wi-Fi connection (default 25).

## 0.3.0

Settings sections and Wi-Fi from the touchscreen.

**Settings**
- Network and Controls moved into **Settings**, which now has four sections behind a segmented control: Display, Network, Touch and System. The tab bar has three tabs: Home, Apps, Settings. The Home Network tile opens Settings > Network. `kiosk-ui --page` takes `display`, `network`, `touch` or `system`.
- **Wi-Fi from the touchscreen.** Settings > Network lists the networks in range. Joining one uses an on-screen keyboard and the admin PIN; saved networks can be reconnected or forgotten, and hidden networks can be added. Networks are saved in `/data/store/wifi.json` and merged into the WPA config at boot, so they survive reboots. `WIFI_SSID` in `kiosk.conf` is now optional: wpa_supplicant always runs, so the list works on an image with no WiFi configured.
- `kioskd`: new requests `wifi_scan`, `wifi_status`, `wifi_connect` and `wifi_forget` (the last two need the admin PIN), and `kioskd wifi-conf`, which prints the saved networks as `wpa_supplicant.conf` blocks.
- `kiosk-config.service` now waits for the `/data` mount, and `kioskd` gives `wpa_cli` a socket directory outside its private `/tmp`. Both were found by testing on a Pi 3B.

**Known limits**
- Tested on a Pi 3B: scanning, a wrong password, a right password and the merge after a reboot. Hidden networks, Change password and Forget are covered by unit tests only. The Pi 4 and Pi 5 images are still built by CI and not run on hardware.
- A board with no WiFi chip would wait for `wlan0` on boot, because wpa_supplicant now always starts.

## 0.2.0

Raspberry Pi 4 and Pi 5, and apps.

**Boards**
- Images for the Raspberry Pi 4 and Pi 5 next to the Pi 3B (`pikiosk-<version>-<board>-sdcard.img.xz`). They are built by CI and have not been run on hardware yet.
- `BOARD=rpi4 ./build.sh` and `BOARD=rpi5 ./build.sh` build them locally. The release also carries the SDK that apps are built against.

**Apps**
- New **Apps** page and a Store behind an admin PIN. Apps are `.kpkg` packages that run one at a time; hold the top-right corner for 3 s to go back to kiosk-ui.
- `kioskd`: root daemon that launches apps as systemd units, installs and removes them atomically (a power cut leaves the old version running), and checks the admin PIN. 3 wrong PINs lock it for `STORE_PIN_LOCKOUT` seconds, across power cuts.
- A new version must stay up 30 s the first time it runs. If it crashes, the kiosk goes back to the previous version, blocks the bad one and shows a notice.
- Signed store: a TUF repository checked for signature, expiry, rollback and hash, only after NTP has set the clock. Set `STORE_URL` and `STORE_CHANNEL` in `kiosk.conf`. Off by default.
- `kpkg`: build, inspect and verify packages, and generate the store catalog.
- `kiosk-kit`: the display, touch and glass code, split out of kiosk-ui as a library for apps.
- New keys in `kiosk.conf`: `STORE_URL`, `STORE_CHANNEL`, `STORE_PIN`, `STORE_PIN_LOCKOUT`, `STORE_ALLOW_UNSIGNED`, `KIOSK_HOME_GESTURE`.

**Changed**
- The repository is a Cargo workspace (`kiosk-ui`, `kiosk-kit`, `kioskd`, `kpkg`). Shared board files moved to `board/common/`.
- The tab bar has five tabs; Apps is second.
- The touch Home gesture reads the panel's multitouch tracking id: the Waveshare panel sends no `BTN_TOUCH`.

**Security**
- Reviewed before release; see docs/app-store.md. Fixed: package-controlled environment variables reaching a root command (hand-back now goes through its own root unit), the socket hiding that broke when `kioskd` restarted, unbounded socket requests, hostile icons, store downgrades, and a wrong PIN ending a live admin session.

**Known limits**
- An app can block the Home gesture by grabbing the touch device, and there is no memory or disk cap for apps (the kernel's memory cgroup is off).
- The store's signing keys live on the maintainer's machine; the root key should be moved offline. Rotating keys means publishing a new root and, for devices already deployed, a new image.
- No installs from cached metadata while offline, no signed packages from USB sticks, and `min_os` is not enforced yet.

## 0.1.0

First release: Raspberry Pi 3B image with kiosk-ui, touch calibration and a read-only root.
