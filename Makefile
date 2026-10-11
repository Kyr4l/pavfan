# pavfan: build, test, package, install.
#   make            build release binaries
#   make test       unit tests + clippy (warnings are errors)
#   make rpm        target/pkg/pavfan-*.rpm   (cargo generate-rpm; no rpmbuild needed)
#   make deb        target/pkg/pavfan_*.deb   (cargo deb; no dpkg needed)
#   make install    manual install into DESTDIR (same file list as the packages)
CARGO   ?= cargo
DESTDIR ?=
PREFIX  ?= /usr
PKGDIR  := target/pkg
BINS    := pavfan pavfand pavfan-tray
ICONS   := off idle active hot error

.PHONY: all build test rpm deb packages install uninstall clean

all: build

build:
	$(CARGO) build --release --workspace

test:
	$(CARGO) test --workspace
	$(CARGO) clippy --workspace --all-targets -- -D warnings

rpm: build
	mkdir -p $(PKGDIR)
	$(CARGO) generate-rpm -p crates/pavfan -o $(PKGDIR)/

deb: build
	mkdir -p $(PKGDIR)
	$(CARGO) deb -p pavfan --no-build -o $(PKGDIR)/

packages: rpm deb
	@ls -la $(PKGDIR)

install: build
	for b in $(BINS); do install -Dm755 target/release/$$b $(DESTDIR)$(PREFIX)/bin/$$b; done
	install -Dm644 packaging/systemd/pavfand.service      $(DESTDIR)$(PREFIX)/lib/systemd/system/pavfand.service
	install -Dm644 packaging/dbus/org.pavfan.Daemon1.conf $(DESTDIR)$(PREFIX)/share/dbus-1/system.d/org.pavfan.Daemon1.conf
	install -Dm644 packaging/modules-load.d/pavfan.conf   $(DESTDIR)$(PREFIX)/lib/modules-load.d/pavfan.conf
	install -Dm644 packaging/modprobe.d/pavfan-ec_sys.conf $(DESTDIR)$(PREFIX)/lib/modprobe.d/pavfan-ec_sys.conf
	install -Dm644 packaging/config/config.toml           $(DESTDIR)/etc/pavfan/config.toml
	install -Dm644 packaging/config/curve.toml            $(DESTDIR)/etc/pavfan/curve.toml
	install -Dm644 packaging/xdg/pavfan-tray.desktop      $(DESTDIR)$(PREFIX)/share/applications/pavfan-tray.desktop
	install -Dm644 packaging/icons/pavfan.svg             $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/pavfan.svg
	for i in $(ICONS); do install -Dm644 packaging/icons/pavfan-$$i.svg $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/status/pavfan-$$i.svg; done
	install -Dm644 README.md $(DESTDIR)$(PREFIX)/share/doc/pavfan/README.md
	install -Dm644 LICENSE   $(DESTDIR)$(PREFIX)/share/licenses/pavfan/LICENSE
	for d in docs/*.md; do install -Dm644 $$d $(DESTDIR)$(PREFIX)/share/doc/pavfan/$$d; done

uninstall:
	-systemctl disable --now pavfand.service
	for b in $(BINS); do rm -f $(DESTDIR)$(PREFIX)/bin/$$b; done
	rm -f $(DESTDIR)$(PREFIX)/lib/systemd/system/pavfand.service \
	      $(DESTDIR)$(PREFIX)/share/dbus-1/system.d/org.pavfan.Daemon1.conf \
	      $(DESTDIR)$(PREFIX)/lib/modules-load.d/pavfan.conf \
	      $(DESTDIR)$(PREFIX)/lib/modprobe.d/pavfan-ec_sys.conf \
	      $(DESTDIR)$(PREFIX)/share/applications/pavfan-tray.desktop \
	      $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/pavfan.svg
	for i in $(ICONS); do rm -f $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/status/pavfan-$$i.svg; done
	rm -rf $(DESTDIR)$(PREFIX)/share/doc/pavfan $(DESTDIR)$(PREFIX)/share/licenses/pavfan

clean:
	$(CARGO) clean
