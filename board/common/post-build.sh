#!/bin/sh
# pikiosk post-build: wires the read-only rootfs to /run and /data,
# removes every way to log in locally, enables the kiosk units.
set -eu

T="${TARGET_DIR}"
BOARD="${2:?post-build: board name missing, set BR2_ROOTFS_POST_SCRIPT_ARGS}"
SYSD="$T/etc/systemd/system"

# ---- which board this image is for (rpi3, rpi4 or rpi5)
echo "$BOARD" > "$T/etc/pikiosk-board"

# ---- mount points --------------------------------------------------------
mkdir -p "$T/boot" "$T/data"
sed -i '\#^/dev/mmcblk0p[13][[:space:]]#d' "$T/etc/fstab"
cat >> "$T/etc/fstab" <<'FSTAB'
/dev/mmcblk0p1  /boot  vfat  ro,nofail,x-systemd.device-timeout=10s  0  0
/dev/mmcblk0p3  /data  ext4  defaults,noatime,nofail,x-systemd.device-timeout=10s  0  2
FSTAB

# ---- runtime-generated config: symlinks into /run/kiosk ---------------------
ln -sfn /run/kiosk/localtime "$T/etc/localtime"
mkdir -p "$T/etc/wpa_supplicant"
ln -sfn /run/kiosk/wpa_supplicant-wlan0.conf "$T/etc/wpa_supplicant/wpa_supplicant-wlan0.conf"
mkdir -p "$T/root"
chmod 0700 "$T/root"
rm -rf "$T/root/.ssh"
ln -sfn /run/kiosk/ssh "$T/root/.ssh"
# persistent SSH host keys
rm -rf "$T/etc/dropbear"
ln -sfn /data/ssh "$T/etc/dropbear"

# ---- kernel modules: drop filesystems a kiosk never mounts ---------------------
# The kernel is built with nearly everything as a module (about 100 MB, of which
# the Pi loads 29). These file systems are 16 MB of the 256 MB root partition.
# Kept: vfat, ext4 (built in), exfat, ntfs3, nfs, squashfs, overlayfs, erofs, isofs, udf, fuse.
for kdir in "$T"/usr/lib/modules/*/; do
	[ -d "$kdir/kernel/fs" ] || continue
	for fs in 9p bcachefs btrfs cachefiles ceph dlm ecryptfs gfs2 hfs hfsplus jffs2 jfs \
		  nfsd nilfs2 ocfs2 reiserfs smb ubifs xfs; do
		rm -rf "$kdir/kernel/fs/$fs"
	done
	if [ -x "${HOST_DIR:-}/sbin/depmod" ]; then
		"$HOST_DIR/sbin/depmod" -a -b "$T" "$(basename "$kdir")"
	else
		echo "post-build: warning: host depmod not found, modules.dep still lists the removed modules" >&2
	fi
done

# ---- no local login, no consoles, no keyboard shortcuts ---------------------
for unit in getty@.service serial-getty@.service autovt@.service \
	    console-getty.service container-getty@.service \
	    debug-shell.service ctrl-alt-del.target; do
	ln -sfn /dev/null "$SYSD/$unit"
done
rm -rf "$SYSD/getty.target.wants"

# ---- enable units --------------------------------------------------------
enable_unit() { # enable <unit> <target>
	unit=$1 target=$2 base=$1
	case "$unit" in *@*.service) base="${unit%%@*}@.service" ;; esac
	for dir in /etc/systemd/system /usr/lib/systemd/system; do
		if [ -e "$T$dir/$base" ]; then
			mkdir -p "$SYSD/$target.wants"
			ln -sfn "$dir/$base" "$SYSD/$target.wants/$unit"
			return 0
		fi
	done
	echo "post-build: warning: $base not found, not enabled" >&2
}
enable_unit kiosk-config.service sysinit.target
enable_unit grow-data.service local-fs-pre.target
enable_unit kiosk.service multi-user.target
enable_unit kioskd.service multi-user.target
enable_unit dropbear.service multi-user.target
enable_unit kiosk-wifi.service multi-user.target
enable_unit systemd-networkd.service multi-user.target
enable_unit systemd-resolved.service multi-user.target
enable_unit systemd-timesyncd.service sysinit.target

# wpa_supplicant's own default config is not used
rm -f "$T/etc/wpa_supplicant.conf"
