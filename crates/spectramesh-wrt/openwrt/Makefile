# OpenWRT package for spectrameshd. Add this repository as a feed to build it;
# see ../README.md.

include $(TOPDIR)/rules.mk

PKG_NAME:=spectramesh
PKG_VERSION:=0.1.0
PKG_RELEASE:=1

PKG_SOURCE_PROTO:=git
PKG_SOURCE_URL:=https://github.com/SpectraMeshCore/spectramesh.git
# Development builds follow main. Pin a commit and set PKG_MIRROR_HASH for releases.
PKG_SOURCE_VERSION:=main
PKG_MIRROR_HASH:=skip

PKG_LICENSE:=Apache-2.0 OR MIT
PKG_LICENSE_FILES:=LICENSE-APACHE LICENSE-MIT

PKG_BUILD_DEPENDS:=rust/host
PKG_BUILD_PARALLEL:=1

include $(INCLUDE_DIR)/package.mk
include $(TOPDIR)/feeds/packages/lang/rust/rust-package.mk

define Package/spectramesh
  SECTION:=net
  CATEGORY:=Network
  SUBMENU:=Routing and Redirection
  TITLE:=SpectraMesh multi-band mesh routing daemon
  URL:=https://github.com/SpectraMeshCore/spectramesh
  DEPENDS:=$(RUST_ARCH_DEPENDS)
endef

define Package/spectramesh/description
  Routes across Ethernet, fiber and Wi-Fi (including 802.11s mesh and
  HaLow) as one mesh network, using Babel-style loop-free routing.
endef

define Package/spectramesh/conffiles
/etc/config/spectramesh
endef

define Build/Compile
	$(call Build/Compile/Cargo,crates/spectramesh-wrt)
endef

define Package/spectramesh/install
	$(INSTALL_DIR) $(1)/usr/sbin $(1)/etc/init.d $(1)/etc/config
	$(INSTALL_BIN) $(PKG_INSTALL_DIR)/bin/spectrameshd $(1)/usr/sbin/
	$(INSTALL_BIN) ./files/spectramesh.init $(1)/etc/init.d/spectramesh
	$(INSTALL_CONF) ./files/spectramesh.config $(1)/etc/config/spectramesh
endef

$(eval $(call BuildPackage,spectramesh))
