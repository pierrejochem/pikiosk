#!/usr/bin/env bash
# Build the pikiosk SD card image inside Docker (macOS or Linux).
#
#   ./build.sh                 full build -> out/sdcard.img
#   ./build.sh menuconfig      change Buildroot options, then ./build.sh savedefconfig
#   ./build.sh savedefconfig   write your changes back to configs/pikiosk_rpi3_defconfig
#   ./build.sh with qt6        merge configs/fragments/qt6.fragment, then build
#   ./build.sh app             rebuild only kiosk-ui -> out/kiosk-ui (scp it to /data/app)
#   ./build.sh shell           shell in the build container
#   ./build.sh reconfig        re-apply defconfig + kernel fragment to an existing build, then build
#   ./build.sh clean           wipe build output (keeps downloads + ccache)
set -euo pipefail

BUILDROOT_VERSION=2026.08
IMAGE=pikiosk-builder
VOLUME=pikiosk-work
HERE="$(cd "$(dirname "$0")" && pwd)"

docker build -q -t "$IMAGE" "$HERE/docker" >/dev/null

run() {
	docker run --rm -it \
		-v "$HERE":/src:ro \
		-v "$VOLUME":/work \
		-e BR2_DL_DIR=/work/dl \
		"$IMAGE" bash -c "
			set -e
			if [ ! -d /work/buildroot ]; then
				git clone --depth 1 -b $BUILDROOT_VERSION https://gitlab.com/buildroot.org/buildroot.git /work/buildroot
			fi
			cd /work/buildroot
			if [ ! -f /work/output/.config ]; then
				make BR2_EXTERNAL=/src O=/work/output pikiosk_rpi3_defconfig
			fi
			$1"
}

case "${1:-build}" in
	build)
		run "make O=/work/output"
		mkdir -p "$HERE/out"
		docker run --rm -v "$VOLUME":/work -v "$HERE/out":/out "$IMAGE" \
			cp /work/output/images/sdcard.img /out/sdcard.img
		echo "Image: $HERE/out/sdcard.img"
		;;
	reconfig)
		# build.sh only applies the defconfig when the volume has no .config;
		# use this after editing the defconfig or board/rpi3/linux.fragment
		run "make BR2_EXTERNAL=/src O=/work/output pikiosk_rpi3_defconfig && make O=/work/output linux-reconfigure"
		exec "$0" build
		;;
	with)
		frag="configs/fragments/${2:?usage: ./build.sh with qt6|sdl2|flutter}.fragment"
		[ -f "$HERE/$frag" ] || { echo "no such fragment: $frag" >&2; exit 1; }
		run "support/kconfig/merge_config.sh -m -O /work/output /work/output/.config /src/$frag && make O=/work/output olddefconfig"
		exec "$0" build
		;;
	app)
		run "make O=/work/output kiosk-ui-rebuild"
		mkdir -p "$HERE/out"
		docker run --rm -v "$VOLUME":/work -v "$HERE/out":/out "$IMAGE" \
			cp /work/output/target/usr/bin/kiosk-ui /out/kiosk-ui
		echo "App: $HERE/out/kiosk-ui"
		;;
	menuconfig|linux-menuconfig)
		run "make O=/work/output $1"
		;;
	savedefconfig)
		# /src is mounted read-only; write to the volume, then copy out
		run "make O=/work/output savedefconfig BR2_DEFCONFIG=/work/pikiosk_rpi3_defconfig"
		docker run --rm -v "$VOLUME":/work -v "$HERE/configs":/out "$IMAGE" \
			cp /work/pikiosk_rpi3_defconfig /out/pikiosk_rpi3_defconfig
		echo "Updated configs/pikiosk_rpi3_defconfig"
		;;
	shell)
		run "exec bash"
		;;
	clean)
		docker run --rm -v "$VOLUME":/work "$IMAGE" rm -rf /work/output
		;;
	*)
		sed -n '2,11p' "$0"; exit 1
		;;
esac
