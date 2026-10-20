#!/bin/bash
# pikiosk post-image: assembles boot partition + rootfs + data into sdcard.img
set -euo pipefail

BOARD_DIR="$(dirname "$0")"
GENIMAGE_CFG="${BINARIES_DIR}/genimage.cfg"
GENIMAGE_TMP="${BUILD_DIR}/genimage.tmp"

# user-editable files on the FAT partition
install -m 0644 "${BOARD_DIR}/boot/kiosk.conf" "${BINARIES_DIR}/kiosk.conf"
install -m 0644 "${BOARD_DIR}/boot/authorized_keys.example" "${BINARIES_DIR}/authorized_keys.example"

FILES=()
for i in "${BINARIES_DIR}"/*.dtb "${BINARIES_DIR}"/rpi-firmware/*; do
	FILES+=( "${i#"${BINARIES_DIR}"/}" )
done
KERNEL=$(sed -n 's/^kernel=//p' "${BINARIES_DIR}/rpi-firmware/config.txt")
FILES+=( "${KERNEL}" kiosk.conf authorized_keys.example )

BOOT_FILES=$(printf '\\t\\t\\t"%s",\\n' "${FILES[@]}")
sed "s|#BOOT_FILES#|${BOOT_FILES}|" "${BOARD_DIR}/genimage.cfg.in" > "${GENIMAGE_CFG}"

ROOTPATH_TMP="$(mktemp -d)"
trap 'rm -rf "${ROOTPATH_TMP}"' EXIT
rm -rf "${GENIMAGE_TMP}"

genimage \
	--rootpath "${ROOTPATH_TMP}" \
	--tmppath "${GENIMAGE_TMP}" \
	--inputpath "${BINARIES_DIR}" \
	--outputpath "${BINARIES_DIR}" \
	--config "${GENIMAGE_CFG}"

echo
echo "pikiosk: ${BINARIES_DIR}/sdcard.img is ready to flash"
