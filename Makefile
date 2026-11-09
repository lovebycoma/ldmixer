# lD Mixer: build and install.
#
# Distribution packages (Arch PKGBUILD, Fedora spec, Nix) call
# `make install DESTDIR=... PREFIX=/usr` after building with cargo, so the
# package manager tracks every installed file and removes them again.
#
# Set NO_UDEV_RELOAD=1 to skip reloading udev after a manual install.
#
# A manual install (`sudo make install`) records what it installed in
# $(PREFIX)/share/ldmixer/manifest, which `ldmixer uninstall` or
# `sudo make uninstall` use to remove everything.

PREFIX   ?= /usr/local
BINDIR   ?= $(PREFIX)/bin
DATADIR  ?= $(PREFIX)/share
# udev only reads rules from /usr/lib/udev/rules.d and /etc/udev/rules.d, so a
# /usr/local install puts the rule under /etc.
ifeq ($(PREFIX),/usr)
UDEVDIR  ?= /usr/lib/udev/rules.d
else
UDEVDIR  ?= /etc/udev/rules.d
endif
CARGO    ?= cargo
TARGET   ?= target/release
APPID    := com.lovebycoma.ldmixer

FILES := \
	$(BINDIR)/ldmixer \
	$(DATADIR)/applications/$(APPID).desktop \
	$(DATADIR)/icons/hicolor/scalable/apps/$(APPID).svg \
	$(DATADIR)/metainfo/$(APPID).metainfo.xml \
	$(DATADIR)/licenses/ldmixer/LICENSE \
	$(UDEVDIR)/70-ldmixer.rules \
	$(DATADIR)/ldmixer/manifest

.PHONY: all build install uninstall check clean

all: build

build:
	$(CARGO) build --release --locked

check:
	$(CARGO) test --locked

install:
	install -Dm755 $(TARGET)/ldmixer        "$(DESTDIR)$(BINDIR)/ldmixer"
	install -Dm644 data/$(APPID).desktop      "$(DESTDIR)$(DATADIR)/applications/$(APPID).desktop"
	install -Dm644 data/$(APPID).svg          "$(DESTDIR)$(DATADIR)/icons/hicolor/scalable/apps/$(APPID).svg"
	install -Dm644 data/$(APPID).metainfo.xml "$(DESTDIR)$(DATADIR)/metainfo/$(APPID).metainfo.xml"
	install -Dm644 LICENSE                    "$(DESTDIR)$(DATADIR)/licenses/ldmixer/LICENSE"
	install -Dm644 data/70-ldmixer.rules     "$(DESTDIR)$(UDEVDIR)/70-ldmixer.rules"
	install -d "$(DESTDIR)$(DATADIR)/ldmixer"
	printf '%s\n' $(FILES) > "$(DESTDIR)$(DATADIR)/ldmixer/manifest"
	@if [ -z "$(DESTDIR)$(NO_UDEV_RELOAD)" ] && command -v udevadm >/dev/null; then \
		udevadm control --reload && \
		udevadm trigger --subsystem-match=usb --attr-match=idVendor=2708 || true; \
	fi

uninstall:
	rm -f $(FILES)
	-rmdir "$(DATADIR)/ldmixer" "$(DATADIR)/licenses/ldmixer" 2>/dev/null
	@if command -v udevadm >/dev/null; then udevadm control --reload || true; fi

clean:
	$(CARGO) clean
