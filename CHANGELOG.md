# Changelog

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

**Known limits**
- The store's signing keys live on the maintainer's machine; the root key should be moved offline. Rotating keys means publishing a new root and, for devices already deployed, a new image.
- No installs from cached metadata while offline, no signed packages from USB sticks, and `min_os` is not enforced yet.

## 0.1.0

First release: Raspberry Pi 3B image with kiosk-ui, touch calibration and a read-only root.
