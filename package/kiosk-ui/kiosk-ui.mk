################################################################################
#
# kiosk-ui
#
################################################################################

KIOSK_UI_VERSION = 0.3.0
# the Cargo workspace root: kiosk-ui builds against kiosk-kit by path
KIOSK_UI_SITE = $(BR2_EXTERNAL_PIKIOSK_PATH)
KIOSK_UI_SITE_METHOD = local
KIOSK_UI_LICENSE = MIT
KIOSK_UI_DEPENDENCIES = host-pkgconf libinput mesa3d udev
# don't copy a local development build into the Buildroot build dir
KIOSK_UI_OVERRIDE_SRCDIR_RSYNC_EXCLUSIONS = \
	--exclude /out --exclude /.git --exclude target

KIOSK_UI_CARGO_ENV = \
	PKG_CONFIG="$(PKG_CONFIG_HOST_BINARY)" \
	PKG_CONFIG_ALLOW_CROSS=1

# Buildroot vendors crates at download time, which never runs for a local
# source tree, and its default build passes --offline. Build online instead,
# with the exact versions from Cargo.lock; crates are cached in the download
# directory (br-cargo-home) for later builds.
define KIOSK_UI_BUILD_CMDS
	cd $(@D) && \
	$(TARGET_MAKE_ENV) \
		$(TARGET_CONFIGURE_OPTS) \
		$(PKG_CARGO_ENV) \
		$(KIOSK_UI_CARGO_ENV) \
		cargo build --release --locked --manifest-path Cargo.toml -p kiosk-ui
endef

define KIOSK_UI_INSTALL_TARGET_CMDS
	$(INSTALL) -D -m 0755 $(@D)/target/$(RUSTC_TARGET_NAME)/release/kiosk-ui \
		$(TARGET_DIR)/usr/bin/kiosk-ui
endef

$(eval $(cargo-package))
