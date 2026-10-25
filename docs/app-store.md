# pikiosk App Store Plan

Oct 20, 2026 · @Pierre Jochem

**Status (v0.2.0):** all seven phases are built; the Pi 3B was tested on hardware for phases 3 to 6. The Pi 4 and Pi 5 images are built by CI and still need a first run on hardware. See the status sections at the end for what differs from this plan.

UI sketch: Design canvas "pikiosk Apps page" (https://claude.ai/artifact/DrLwMSn99S25ToFUaZxoon).

## Overview

Add the store to [pikiosk](https://github.com/pierrejochem/pikiosk) as two new Rust crates and one new page in kiosk-ui: `kioskd`, a root daemon that verifies, installs and launches apps; `kpkg`, a CLI that builds and signs packages; and a **Store** page in kiosk-ui that talks to `kioskd` over a Unix socket. Apps live on the `/data` partition, so the read-only root and the reflash-safe layout stay as they are.

**What the repo fixes in place** (from `master` at c66b4ce):

- Raspberry Pi 3B today; this plan adds Pi 4 and Pi 5 images. All three are 64-bit Buildroot 2026.08 builds with glibc and systemd, so one app package runs on all of them
- Root (`/`) and `KIOSKBOOT` (`/boot`) mounted read-only; only `/data` (ext4) is writable
- kiosk-ui draws straight to DRM/KMS + GBM + EGL/GLES2 and reads libinput; there is no X11 or Wayland, so only **one program can own the display at a time**
- `kiosk.service` runs one `APP` from `kiosk.conf` as the unprivileged `kiosk` user, restarts it after 2 s, reboots after 5 crashes in 2 min
- No RTC on the Pi 3B and Pi 4: the clock is wrong until NTP syncs (the Pi 5 has an RTC, kept running by an optional battery)
- 800×480 resistive single-touch panel, no keyboard

**Goals:** an admin installs, updates and removes apps from the touchscreen without SSH or reflashing; anyone can launch installed apps; every package signed; automatic rollback of a broken update; installs from an SD card or USB stick when offline; the same package on Pi 3B, 4 and 5.

**Non-goals for v1:** payments, third-party publishers, OS (rootfs) updates, two apps on screen at once, apps not built with egui on `kiosk-kit`, and booting into a store app: kiosk-ui is always home.

## Architecture

(The original doc embeds a diagram here: store architecture, cloud and device.)

kiosk-ui never writes outside `/data/app` or starts processes: it asks `kioskd`, which verifies packages from the repo or local media, installs them under `/data/store` and starts each app as its own unit. In the diagram, dashed lines show which program draws: kiosk-ui or the app, never both, because KMS has a single owner without a compositor.

## Package format, app model and display handoff

Apps take turns on the display: launching an app stops kiosk-ui, and kiosk-ui comes back when the app exits. Without a compositor this is the only reliable way to hand over DRM master, and it frees kiosk-ui's GPU memory on a 1 GB board.

**Package.** A `.kpkg` is a `tar.zst` archive with a manifest, an aarch64 glibc binary and assets. `kioskd` unpacks it into a versioned directory and switches a symlink, so install and rollback are each one atomic rename on `/data`.

```toml
id = "de.jochem.weather"
version = "1.5.0"                    # semver
name = "Weather"
entry = "bin/weather"
icon = "assets/icon.png"             # 128x128 PNG, shown on the glass launcher
min_os = "0.2"                       # pikiosk release
boards = ["rpi3", "rpi4", "rpi5"]    # optional; default: all
permissions = ["network"]            # network, audio, gpio, serial
args = []
env = []
```

**On-disk layout** (all on the `kioskdata` partition)

| Path | Holds |
| --- | --- |
| `/data/store/apps/<id>/<version>/` | Unpacked package, made read-only after verify |
| `/data/store/apps/<id>/current` | Symlink to the active version; the previous one is kept |
| `/data/store/data/<id>/` | The app's writable state, survives updates and reflashes |
| `/data/store/cache/` | Downloaded packages and TUF metadata |
| `/data/store/state.json` | Installed set, pinned versions, pending health checks |
| `/data/app/` | kiosk-ui's own settings, unchanged |

**Launching.** Each app runs from a template unit `kiosk-app@<id>.service` shipped in the rootfs overlay:

- `Conflicts=kiosk.service`: starting the app stops kiosk-ui, which releases the display
- `OnSuccess=kiosk.service` and `OnFailure=kiosk.service`: kiosk-ui returns when the app ends
- `User=kiosk`, `SupplementaryGroups=video render input` (+ `audio` if declared)
- `TemporaryFileSystem=/data:ro` + `BindPaths=/data/store/data/%i:/data/app`: the app sees only its own state, at the same `/data/app` path existing pikiosk apps already use
- `ProtectSystem=strict`, `NoNewPrivileges=yes`, `PrivateTmp=yes`, `PrivateNetwork=yes` unless `network` is declared (set by a drop-in `kioskd` writes to `/run/systemd/system/kiosk-app@<id>.service.d/`)
- The same crash-loop limits as `kiosk.service`, but a crash loop falls back to kiosk-ui instead of rebooting

**Watch the start limit.** `kiosk.service` counts every start toward `StartLimitBurst=5` in 120 s with `StartLimitAction=reboot`, so opening five apps in two minutes would reboot the board. `kioskd` runs `systemctl reset-failed kiosk.service` after each clean handback, or the limit moves into `kioskd` and counts only real crashes.

**Getting home.** `kioskd` reads the touch device through evdev, read-only and without grabbing it. While an app runs, a 3-second press in the **top-right corner** (a 96×96 px area: on the resistive panel a 64 px target was missed in a field test) counts as Home: `kioskd` stops the app unit and kiosk-ui starts. This works even when an app hangs. Raw evdev coordinates are uncalibrated, so `kioskd` applies the same calibration matrix kiosk-ui uses (the saved one from `/data/app/kiosk-ui.conf`, else the image's udev default). `KIOSK_HOME_GESTURE=no` in `kiosk.conf` turns it off. An app can also go home by exiting with status 0.

**kiosk-ui is always home.** Store apps start only from the Apps page, never at boot. `APP=` in `kiosk.conf` stays as it is for development (scp a binary to `/data/app`).

**Apps are egui only.** Move `kms.rs`, `gfx.rs`, the glass renderer and widgets out of kiosk-ui into a library crate `kiosk-kit`. A store app is its `App` impl plus `kiosk_kit::run(app)`, and gets the same display handling, touch calibration and look as kiosk-ui. The admin curates the catalog, so "egui on kiosk-kit" is a review rule rather than a technical check; the image ships no SDL2, Qt or Flutter runtime for store apps.

## Security and updates

Use The Update Framework (TUF) through the `tough` crate rather than a home-made signature scheme: it also stops rollback to old versions, frozen metadata and a bad mirror. Two pikiosk specifics shape it: the board has no RTC, and the root filesystem cannot be written.

- **Trust anchor:** `root.json` is baked into the rootfs at `/usr/share/kioskd/root.json` by the Buildroot package; changing it means a new image. Root key offline, targets key in CI, timestamp key on the server.
- **No clock before NTP:** TUF rejects expired metadata, and until `systemd-timesyncd` syncs, a Pi 3B or Pi 4 thinks it is 1970 or the build date. `kioskd` checks for updates only after `/run/systemd/timesync/synchronized` exists, on every board including the Pi 5 so all three behave alike. Installs from cache or local media check signatures and hashes but skip the expiry check.
- **Transport:** `tough`'s HTTP transport (`reqwest` + `rustls`, verifying with the system certificates). The image therefore ships `ca-certificates`; without a CA bundle the HTTP client cannot even be built, so the store is unusable without it. (Phase 5 change: the first draft compiled the webpki roots in.)
- **Verify before unpack:** length and hash from targets metadata, then the manifest: id matches, `min_os` ≤ this image, this board is in `boards`, permissions no wider than the catalog approved.
- **Atomic install on power loss:** download to `cache/` → verify → unpack to `<version>.tmp/` → `fsync` files and directory → rename to `<version>/` → swap `current`. Pulling the plug at any step leaves the old version running, matching the README's "pulling the power is safe".
- **Health gate:** a new version must stay up 30 s, or call `ready` on the socket, before it counts as good; otherwise `kioskd` flips `current` back, blocks that version and kiosk-ui shows the rollback notice.
- **Admin PIN:** `apply-config` writes `STORE_PIN` to a root-only file under `/run/kiosk`; `kioskd` checks it, kiosk-ui never sees it. After 3 wrong PINs `kioskd` refuses every PIN for `STORE_PIN_LOCKOUT` seconds (default 300 = 5 min). Without an RTC wall-clock time is unreliable, so the remaining lockout is counted on the boot clock and saved to `state.json` every 10 s; pulling the power does not reset it.
- **Local media:** `.kpkg` files in `/boot/apps/` (put there from a Mac like `kiosk.conf`) or on a USB stick are offered for install at boot; they must be signed by the same targets key unless `STORE_ALLOW_UNSIGNED=yes` is set for development.
- **Permission changes:** an update that adds a permission waits for the admin to approve it on the device.

## Store UI in kiosk-ui

Add one page, **Apps**, to kiosk-ui's floating tab bar (Home · Apps · Settings), built from the existing glass widgets in `widgets.rs` and laid out with `Lay` for both the 800×480 and the 1280×720 designs. kiosk-ui never installs anything itself: it sends requests to `kioskd` and draws the events that come back.

| View | Content | Who |
| --- | --- | --- |
| Apps (launcher) | Glass tiles of installed apps, plus the Store tile that opens the PIN keypad; tap to launch | Everyone |
| Catalog | Available apps by category, Install buttons | Admin |
| App detail | Icon, version, size, permissions, Update / Remove / Roll back | Admin |
| Progress | Download %, verifying, installing; a rollback notice | Admin |
| Store settings (in Settings) | Channel, Check now, last sync, unlock PIN | Admin |

**Touch and kiosk details**

- Resistive single-touch with ~20 px wobble: launcher tiles at least 96 px, no long-press menus, scroll only by swiping the page like the existing horizontal page swipe.
- No keyboard: no search field. Browse by category, and enter the admin PIN on a glass numeric keypad.
- Wrong PIN: the dots shake and clear. After the third wrong PIN the keypad shows a countdown and stays disabled until `kioskd` lifts the lockout.
- Admin mode locks again after 60 s without a touch.
- The status bar's top-right corner (Wi-Fi, clock) is the Home gesture area inside apps; kiosk-ui puts no buttons there, so the gesture is never learned as a tap target.
- The kms loop sleeps between frames (1 Hz with animation off), so socket events must wake it: add the socket fd to the loop's poll set, or an `eventfd` the client thread writes to.
- Load icons once with the `image` crate kiosk-ui already uses and keep the GL textures; uploads are slow on VC4.
- Before a launch, kiosk-ui saves settings and exits cleanly, so the handoff never looks like a crash.

**Tests:** extend the existing offscreen UI tests in `headless.rs` with a fake `kioskd` on a temporary socket: install, progress, launch request and the rollback notice, at both screen sizes. `kiosk-ui --screenshot --page apps` adds the page to the docs screenshots. To click through it by hand, `tools/emulate.sh` runs kiosk-ui and the real `kioskd` in a desktop window (PIN `1234`, a stand-in `wpa_cli` for the Wi-Fi list); see the README.

**Offline:** the catalog shows the cached index with a "last synced" time, and Install is greyed out for packages not in the cache.

## Backend and publishing pipeline

No server to run at first: a TUF repository is static files, so GitHub Pages (or any bucket) serves it, and GitHub Actions builds and signs into it, next to the existing `ci.yml` and `image.yml`.

**Where things live**

- `pikiosk` (this repo): `kioskd`, `kpkg`, `kiosk-kit`, the Store page, units, Buildroot packages, the TUF `root.json`
- `pikiosk-apps` (new repo): one folder per app, the publish workflow, and the TUF repo on its `gh-pages` branch

**Publish flow**

1. Tag `weather-v1.5.0` in `pikiosk-apps`.
2. CI builds in the **pikiosk SDK**: `image.yml` runs Buildroot's `make sdk` and attaches the SDK tarball to each pikiosk release. Its aarch64 sysroot has the glibc, libinput, libudev and Mesa GBM/EGL that kiosk-kit links against, so `cargo build --target aarch64-unknown-linux-gnu` with the SDK's linker gives one binary for all three boards. Plain `cargo zigbuild` cannot do this: it has no sysroot with those libraries.
3. `kpkg build` assembles the package and lints the manifest (entry is executable, icon size, `boards` known).
4. `kpkg catalog` lists the packages as `catalog.json`, which is signed as a TUF target next to the packages; `scripts/publish.sh` runs `tuftool create` or `update` to sign targets, snapshot and timestamp for the `beta` channel. The keys are GitHub Actions secrets, raw Ed25519 PKCS#8 DER files (`tough` reads Ed25519 keys as DER, not PEM). The repository layout is `<STORE_URL>/<channel>/{metadata,targets}`. (Phase 5 change: there is no `kpkg sign`; `tuftool` does the signing.)
5. A scheduled workflow re-signs `timestamp.json` daily so devices never see it expire.
6. Promote to `stable` with a manual workflow run, which re-signs the packages of `beta` as the `stable` channel.

**Phase 5 status.** Built and tested: the TUF client in `kioskd` (signature, expiry, rollback, length and hash checks, NTP gate), `kpkg catalog`, the publish scripts and workflow in `pikiosk-apps`. Not built yet: installing from the cached metadata while offline, signed packages from local media (an offline repository mirror), the `min_os` check, and the `STORE_PIN`. The key ceremony has been run: the `root.json` in the image is the production trust anchor (one Ed25519 key per role, threshold 1, valid for five years).

**Device config:** new whitelisted keys in `apply-config` and `kiosk.conf`: `STORE_URL=` (empty = store off, the default), `STORE_CHANNEL=stable`, `STORE_PIN=`, `STORE_PIN_LOCKOUT=300` (seconds after 3 wrong PINs), `KIOSK_HOME_GESTURE=yes`.

**Later, if a fleet needs it:** a small Axum service for per-device channels and install reports. Until then, the channel in `kiosk.conf` is enough.

## Changes to the pikiosk repo

The one structural change is a Cargo workspace at the repo root, because kiosk-ui and the new crates share `kiosk-kit` by path and Buildroot's local `SITE` copies only one directory.

| Path | Change |
| --- | --- |
| `Cargo.toml` (new) | Workspace: `kiosk-ui`, `kiosk-kit`, `kioskd`, `kpkg`; one `Cargo.lock` |
| `kiosk-kit/` (new) | `kms.rs`, `gfx.rs`, `glass.rs`, `widgets.rs`, `calibrate.rs` moved out of kiosk-ui |
| `kioskd/` (new) | Daemon: socket API, TUF client, installer, launcher, PIN lockout, evdev home gesture |
| `kpkg/` (new) | Host CLI: `build`, `sign`, `inspect`, `verify` |
| `kiosk-ui/src/store.rs` (new) | Apps page, Store views, PIN keypad and socket client; `app.rs` gains the Apps tab second |
| `package/kioskd/` (new) | Buildroot `cargo-package`; installs `/usr/sbin/kioskd` and `/usr/share/kioskd/root.json` |
| `package/kiosk-ui/kiosk-ui.mk` | `SITE` = repo root (excluding `out/`), build with `-p kiosk-ui` |
| `rootfs-overlay/.../kioskd.service` (new) | Root service, `After=data.mount`, socket `/run/kioskd/kioskd.sock` group `kiosk` mode 0660 |
| `rootfs-overlay/.../kiosk-app@.service` (new) | The app template unit described above |
| `post-build.sh` | Enable `kioskd.service`; write `/etc/pikiosk-board` (`rpi3`, `rpi4` or `rpi5`) for the `boards` check |
| `apply-config`, `boot/kiosk.conf` | `STORE_URL`, `STORE_CHANNEL`, `STORE_PIN`, `STORE_PIN_LOCKOUT`, `KIOSK_HOME_GESTURE` |
| `board/rpi4/`, `board/rpi5/` (new) | `config.txt`, `cmdline.txt`, `linux.fragment`, `genimage.cfg.in`; the shared overlay and scripts move to `board/common/` |
| `configs/pikiosk_rpi4_defconfig`, `pikiosk_rpi5_defconfig` (new) | Kernel `bcm2711` (Pi 4) and `bcm2712` (Pi 5); Mesa gets the V3D Gallium driver for the GPU while VC4 keeps driving the display; firmware and DTB settings taken from Buildroot's own `raspberrypi4_64` and `raspberrypi5` defconfigs |
| `configs/pikiosk_rpi3_defconfig` | `BR2_PACKAGE_KIOSKD=y`; check the 256 MB rootfs still fits |
| `.github/workflows/ci.yml` | Build and test the whole workspace; a `kpkg` build → sign → verify round trip |
| `.github/workflows/image.yml` | Matrix over `rpi3`, `rpi4`, `rpi5`; images named per board; the SDK tarball attached to the release |
| `README.md` | Store section, `kiosk.conf` keys, supported boards, how to publish an app |

**Pi 4 and Pi 5 display:** on these boards the display (VC4) and the GPU (V3D) are separate DRM devices. kiosk-ui's `open_output()` already scans `/dev/dri/card0`–`card7` for a connected output, so it should find the VC4 card unchanged, with Mesa routing GLES rendering to V3D. The Pi 4 and Pi 5 also have micro-HDMI, so the Waveshare panel needs an adapter cable; the HDMI connector is still named `HDMI-A-1`. Both points need checking on hardware in phase 2.

**Unchanged on purpose:** the partition layout, the read-only root, the hardware watchdog and crash-loop reboot for kiosk-ui, and `APP=/data/app/myapp` for quick scp development. OS updates stay "reflash the SD card"; an A/B rootfs (a second root partition in `genimage.cfg.in` plus RAUC) is a separate project.

## Roadmap

Each phase is a PR that leaves `master` releasable; move on when its check passes on a real Pi 3B.

1. **Workspace and kiosk-kit**: root workspace, move the KMS backend and widgets into `kiosk-kit`, Buildroot package pointed at the root. Check: CI green, the Pi 3B image boots into the same kiosk-ui.
2. **Pi 4 and Pi 5 images**: `board/common/`, the two new board directories and defconfigs, `image.yml` matrix, SDK tarball on the release. Check: kiosk-ui boots, draws and takes touch on all three boards.
3. **Display handoff**: `kiosk-app@.service`, start-limit fix, `kioskd` with only `launch`/`stop`, top-right home gesture. Check: launch a test app, hold the top-right corner, kiosk-ui returns; five launches in a minute do not reboot.
4. **Local install**: `kpkg build`, unsigned installs from `/boot/apps/` behind `STORE_ALLOW_UNSIGNED`, Apps launcher page. Check: pull the power mid-install and the old version still runs.
5. **Signed store**: TUF client, `pikiosk-apps` repo with the SDK-based publish workflow, `STORE_*` keys, NTP gate. Check: a tampered package and an old snapshot are rejected; a board without network still boots and launches.
6. **Catalog UI, PIN and rollback**: catalog, detail, progress, PIN keypad with lockout, health gate, rollback notice. Check: 3 wrong PINs lock for `STORE_PIN_LOCKOUT` and survive a power cut; an app that crashes on start is rolled back automatically.
7. **Docs and release**: README section, screenshots, `v0.x` images for all three boards with the store off by default.

## Decisions (20 Oct 2026)

| Question | Decision |
| --- | --- |
| Who installs | Admin only, with the PIN; anyone can launch |
| Boot app | kiosk-ui is always home; no `APP=store:<id>` |
| Home gesture | 3 s hold in the top-right corner (96×96 px) |
| App runtimes | egui on `kiosk-kit` only |
| Boards | Pi 3B, plus Pi 4 and Pi 5 |
| Apps tab | Second, between Home and Network |
| Store tile | Shown to everyone; tapping it asks for the PIN |
| Wrong PIN | 3 tries, then locked for `STORE_PIN_LOCKOUT` seconds, default 300 |

## Phase 6 status

Built and checked on a Pi 3B: the admin PIN with lockout, the health gate with automatic rollback, and the Store screens in kiosk-ui (PIN keypad, catalog, app detail with install progress, rollback notice).

- **PIN and token.** `kioskd` checks the PIN against a root-only file written from `STORE_PIN` (4 to 8 digits; empty means nobody can be admin). A correct PIN returns a random token that kiosk-ui sends with install, update, remove and roll-back requests. It ends 60 s after its last use; every touch extends it. Root on the device (an SSH session) needs no token. After 3 wrong PINs every PIN is refused for `STORE_PIN_LOCKOUT` seconds. The wrong-PIN count is saved at once and the remaining lockout every 10 s, so a power cut keeps the lock: after a hard reset with 98 s left, the board came back with 98 s left.
- **Apps cannot reach `kioskd`.** `kiosk-app@.service` hides `/run/kioskd` (the socket's directory) and `/boot` from the app (the PIN is in `kiosk.conf`). For the same reason an app cannot call `ready`, which the first draft planned: the health gate is the 30 s stay-up rule only.
- **Health gate.** It starts the first time a new version runs. If the unit crashes (or waits to restart after a failure) within 30 s, `kioskd` stops it, switches back to the previous version, deletes and blocks the bad one and stores a notice. An app that exits with status 0 or is stopped early stays pending, and the next launch tries again. Test: an update that crashes on start was rolled back about one second after it was launched.
- **Install progress.** The install request blocks, and kiosk-ui polls a `progress` request about three times a second (download bytes, then unpack position).
- **Not built:** the Store settings block in Settings (the catalog header has the channel, last sync and a Check now button instead).

## Security review (before v0.2.0)

An independent read of the store code, `kioskd` and the unit files found these. Each was checked against the code; two were reproduced on the Pi.

Fixed:
- **A package could get root through `env`.** `ExecStopPost=+` runs as root outside the sandbox but inherits the unit's `Environment=`, which a package sets, so `LD_PRELOAD` loaded the package's own library as root. Putting `env -i` in front does not help (the `env` program itself loads the library first), so no root command runs from the app unit any more: the app hands over to a separate root unit, `kiosk-handback.service`, which has its own clean environment, clears kiosk-ui's start limit and starts kiosk-ui. As a second layer `kpkg` refuses `LD_*`, `GLIBC_*`, `SYSTEMD_*`, `MALLOC_*`, `KIOSK_*`, `PATH`, `HOME` and similar keys.
- **The socket block broke when `kioskd` restarted.** Hiding a single socket file is undone when the file is replaced (reproduced on the Pi: the sandbox saw the real socket after a restart). The socket now lives in `/run/kioskd`, a directory that is kept across restarts and hidden from apps.
- **Socket requests had no bounds.** A request line is now limited to 4 KiB and 10 s, and at most 16 connections are served at once.
- **A hostile icon could crash-loop kiosk-ui into the reboot limit.** Icons are checked at install (regular file, at most 64 KiB, PNG header, 128×128), and kiosk-ui refuses oversized ones before decoding.
- **A wrong PIN ended a live admin session.** It no longer does. If the failed-attempt count cannot be saved (disk full), kioskd now locks instead of forgetting it.
- **The store could downgrade.** An admin install now refuses a store version older than the installed one.
- Packages with more than 10,000 entries are refused, and the temporary copy of `kiosk.conf` is private.
- Apps get `TasksMax=128`, `CPUWeight=50` and `OOMScoreAdjust=500`; kioskd and kiosk-ui are protected from the OOM killer.

Already handled: a rewound clock cannot revive expired metadata, because `tough` keeps the latest known time in its datastore and refuses to run if the clock steps back.

Known limits, not fixed:
- **An app can block the Home gesture.** Apps need the `input` group to read touch, and an app can take an exclusive grab on the touch device, so `kioskd` no longer sees the press. Recovery is a power cycle or SSH. The mitigation is curation: only the admin's signed catalog installs apps. A real fix would have `kioskd` own the device and forward events.
- **No memory cap for apps.** This kernel's memory cgroup controller is not enabled (`cpuset cpu io pids`). The OOM score adjustment is the only protection until `cgroup_enable=memory` is set and tested.
- **No disk quota.** An app can fill `/data`, which breaks installs and state writes.
- **Apps and kiosk-ui share the `kiosk` uid.** A compromised kiosk-ui can read `/boot/kiosk.conf` (the PIN and WiFi password). Apps cannot, because their unit hides `/boot`.
- Any process that can reach the socket can lock the admin out by sending wrong PINs.
