#!/usr/bin/env bash
# Build the pikiosk SD card image inside Docker (macOS or Linux).
#
#   ./build.sh                 full build -> out/sdcard.img (Raspberry Pi 3B)
#   BOARD=rpi4 ./build.sh      the same for the Pi 4 or Pi 5 (rpi5) -> out/sdcard-rpi4.img
#   ./build.sh menuconfig      change Buildroot options, then ./build.sh savedefconfig
#   ./build.sh savedefconfig   write your changes back to configs/pikiosk_${BOARD}_defconfig
#   ./build.sh with qt6        merge configs/fragments/qt6.fragment, then build
#   ./build.sh app             rebuild only kiosk-ui -> out/kiosk-ui (scp it to /data/app)
#   ./build.sh shell           shell in the build container
#   ./build.sh reconfig        re-apply defconfig + kernel fragment to an existing build, then build
#   ./build.sh clean           wipe build output (keeps downloads + ccache)
set -euo pipefail

BUILDROOT_VERSION=2026.08
# rpi3 (default), rpi4 or rpi5. The Pi 3B keeps its original build directory, so an
# existing build volume is reused; the other boards get their own.
BOARD=${BOARD:-rpi3}
case "$BOARD" in
	rpi3) OUT=/work/output; IMG=sdcard.img ;;
	rpi4|rpi5) OUT=/work/output-$BOARD; IMG=sdcard-$BOARD.img ;;
	*) echo "unknown BOARD '$BOARD' (rpi3, rpi4 or rpi5)" >&2; exit 1 ;;
esac
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
			if [ ! -f $OUT/.config ]; then
				make BR2_EXTERNAL=/src O=$OUT pikiosk_${BOARD}_defconfig
			fi
			$1"
}

case "${1:-build}" in
	build)
		run "make O=$OUT"
		mkdir -p "$HERE/out"
		docker run --rm -v "$VOLUME":/work -v "$HERE/out":/out "$IMAGE" \
			cp $OUT/images/sdcard.img /out/$IMG
		echo "Image: $HERE/out/$IMG"
		;;
	reconfig)
		# build.sh only applies the defconfig when the volume has no .config;
		# use this after editing the defconfig or board/common/linux.fragment
		run "make BR2_EXTERNAL=/src O=$OUT pikiosk_${BOARD}_defconfig && make O=$OUT linux-reconfigure"
		exec "$0" build
		;;
	with)
		frag="configs/fragments/${2:?usage: ./build.sh with qt6|sdl2|flutter}.fragment"
		[ -f "$HERE/$frag" ] || { echo "no such fragment: $frag" >&2; exit 1; }
		run "support/kconfig/merge_config.sh -m -O $OUT $OUT/.config /src/$frag && make O=$OUT olddefconfig"
		exec "$0" build
		;;
	app)
		run "make O=$OUT kiosk-ui-rebuild"
		mkdir -p "$HERE/out"
		docker run --rm -v "$VOLUME":/work -v "$HERE/out":/out "$IMAGE" \
			cp $OUT/target/usr/bin/kiosk-ui /out/kiosk-ui
		echo "App: $HERE/out/kiosk-ui"
		;;
	menuconfig|linux-menuconfig)
		run "make O=$OUT $1"
		;;
	savedefconfig)
		# /src is mounted read-only; write to the volume, then copy out
		run "make O=$OUT savedefconfig BR2_DEFCONFIG=/work/pikiosk_${BOARD}_defconfig"
		docker run --rm -v "$VOLUME":/work -v "$HERE/configs":/out "$IMAGE" \
			cp /work/pikiosk_${BOARD}_defconfig /out/pikiosk_${BOARD}_defconfig
		echo "Updated configs/pikiosk_${BOARD}_defconfig"
		;;
	shell)
		run "exec bash"
		;;
	clean)
		docker run --rm -v "$VOLUME":/work "$IMAGE" rm -rf $OUT
		;;
	*)
		sed -n '2,12p' "$0"; exit 1
		;;
esac
