################################################################################
#
# kioskd
#
################################################################################

KIOSKD_VERSION = 0.1.0
# the Cargo workspace root, like kiosk-ui
KIOSKD_SITE = $(BR2_EXTERNAL_PIKIOSK_PATH)
KIOSKD_SITE_METHOD = local
KIOSKD_LICENSE = MIT
# don't copy a local development build into the Buildroot build dir
KIOSKD_OVERRIDE_SRCDIR_RSYNC_EXCLUSIONS = \
	--exclude /out --exclude /.git --exclude target

# Same online build as kiosk-ui: crates come from Cargo.lock, not from a
# vendored download.
define KIOSKD_BUILD_CMDS
	cd $(@D) && \
	$(TARGET_MAKE_ENV) \
		$(TARGET_CONFIGURE_OPTS) \
		$(PKG_CARGO_ENV) \
		cargo build --release --locked --manifest-path Cargo.toml -p kioskd
endef

define KIOSKD_INSTALL_TARGET_CMDS
	$(INSTALL) -D -m 0755 $(@D)/target/$(RUSTC_TARGET_NAME)/release/kioskd \
		$(TARGET_DIR)/usr/sbin/kioskd
endef

$(eval $(cargo-package))
