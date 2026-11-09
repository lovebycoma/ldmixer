# lD Mixer: build and install.
#
# Distribution packages (Arch PKGBUILD, Fedora spec, Nix) call
# `make install DESTDIR=... PREFIX=/usr` after building with cargo, so the
# package manager tracks every installed file and removes them again.
#
# Set NO_UDEV_RELOAD=1 to skip reloading udev after a manual install.
#
# A manual install (`sudo make install`) records what it installed in
# $(PREFIX)/share/ld-mixer/manifest, which `ld-mixer uninstall` or
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
	$(BINDIR)/ld-mixer \
	$(DATADIR)/applications/$(APPID).desktop \
	$(DATADIR)/icons/hicolor/scalable/apps/$(APPID).svg \
	$(DATADIR)/metainfo/$(APPID).metainfo.xml \
	$(DATADIR)/licenses/ld-mixer/LICENSE \
	$(UDEVDIR)/70-ld-mixer.rules \
	$(DATADIR)/ld-mixer/manifest

.PHONY: all build install uninstall check clean

all: build

build:
	$(CARGO) build --release --locked

check:
	$(CARGO) test --locked

install:
	install -Dm755 $(TARGET)/ld-mixer        "$(DESTDIR)$(BINDIR)/ld-mixer"
	install -Dm644 data/$(APPID).desktop      "$(DESTDIR)$(DATADIR)/applications/$(APPID).desktop"
	install -Dm644 data/$(APPID).svg          "$(DESTDIR)$(DATADIR)/icons/hicolor/scalable/apps/$(APPID).svg"
	install -Dm644 data/$(APPID).metainfo.xml "$(DESTDIR)$(DATADIR)/metainfo/$(APPID).metainfo.xml"
	install -Dm644 LICENSE                    "$(DESTDIR)$(DATADIR)/licenses/ld-mixer/LICENSE"
	install -Dm644 data/70-ld-mixer.rules     "$(DESTDIR)$(UDEVDIR)/70-ld-mixer.rules"
	install -d "$(DESTDIR)$(DATADIR)/ld-mixer"
	printf '%s\n' $(FILES) > "$(DESTDIR)$(DATADIR)/ld-mixer/manifest"
	@if [ -z "$(DESTDIR)$(NO_UDEV_RELOAD)" ] && command -v udevadm >/dev/null; then \
		udevadm control --reload && \
		udevadm trigger --subsystem-match=usb --attr-match=idVendor=2708 || true; \
	fi

uninstall:
	rm -f $(FILES)
	-rmdir "$(DATADIR)/ld-mixer" "$(DATADIR)/licenses/ld-mixer" 2>/dev/null
	@if command -v udevadm >/dev/null; then udevadm control --reload || true; fi

clean:
	$(CARGO) clean
