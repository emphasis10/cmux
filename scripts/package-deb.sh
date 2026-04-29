#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: ./scripts/package-deb.sh --version <version> [--arch <amd64|arm64>] [--output-dir <dir>]

Builds a Debian package from artifacts produced by ./scripts/build-linux.sh.
EOF
}

VERSION=""
ARCH=""
OUTPUT_DIR=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      VERSION="${2:-}"
      shift 2
      ;;
    --arch)
      ARCH="${2:-}"
      shift 2
      ;;
    --output-dir)
      OUTPUT_DIR="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown option $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

if [[ -z "$VERSION" ]]; then
  echo "error: --version is required" >&2
  usage >&2
  exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
DIST_DIR="$PROJECT_DIR/dist/linux"
OUTPUT_DIR="${OUTPUT_DIR:-$PROJECT_DIR/dist/deb}"
ARCH="${ARCH:-$(dpkg --print-architecture)}"
PACKAGE_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/cmux-deb.XXXXXX")"
SHLIBDEPS_DIR="$(mktemp -d "${TMPDIR:-/tmp}/cmux-shlibdeps.XXXXXX")"
trap 'rm -rf "$PACKAGE_ROOT" "$SHLIBDEPS_DIR"' EXIT
chmod 0755 "$PACKAGE_ROOT"

GUI_BIN="$DIST_DIR/cmux-gui"
DAEMON_BIN="$DIST_DIR/cmuxd-remote"

for artifact in "$GUI_BIN" "$DAEMON_BIN"; do
  if [[ ! -x "$artifact" ]]; then
    echo "error: missing build artifact $artifact; run ./scripts/build-linux.sh first" >&2
    exit 1
  fi
done

install -d -m 0755 \
  "$PACKAGE_ROOT/DEBIAN" \
  "$PACKAGE_ROOT/usr/bin" \
  "$PACKAGE_ROOT/usr/lib/cmux" \
  "$PACKAGE_ROOT/usr/share/applications" \
  "$PACKAGE_ROOT/usr/share/metainfo" \
  "$PACKAGE_ROOT/usr/share/doc/cmux" \
  "$PACKAGE_ROOT/usr/share/lintian/overrides" \
  "$PACKAGE_ROOT/usr/share/man/man1" \
  "$OUTPUT_DIR"

install -m 0755 "$GUI_BIN" "$PACKAGE_ROOT/usr/lib/cmux/cmux-gui"
install -m 0755 "$DAEMON_BIN" "$PACKAGE_ROOT/usr/lib/cmux/cmuxd-remote"
if [[ -f "$DIST_DIR/libghostty-gtk-embed.so" ]]; then
  install -m 0755 "$DIST_DIR/libghostty-gtk-embed.so" "$PACKAGE_ROOT/usr/lib/cmux/libghostty-gtk-embed.so"
fi
install -m 0755 "$PROJECT_DIR/linux/packaging/deb/cmux" "$PACKAGE_ROOT/usr/bin/cmux"
install -m 0644 "$PROJECT_DIR/linux/packaging/deb/com.cmuxterm.cmux.desktop" \
  "$PACKAGE_ROOT/usr/share/applications/com.cmuxterm.cmux.desktop"
install -m 0644 "$PROJECT_DIR/linux/packaging/deb/com.cmuxterm.cmux.metainfo.xml" \
  "$PACKAGE_ROOT/usr/share/metainfo/com.cmuxterm.cmux.metainfo.xml"
cat > "$PACKAGE_ROOT/usr/share/doc/cmux/copyright" <<'EOF'
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: cmux
Source: https://github.com/manaflow-ai/cmux

Files: *
Copyright: Copyright (C) 2026 Manaflow and cmux contributors
License: GPL-3+

License: GPL-3+
 This package is free software; you can redistribute it and/or modify it
 under the terms of the GNU General Public License as published by the Free
 Software Foundation; either version 3 of the License, or (at your option)
 any later version.
 .
 On Debian systems, the complete text of the GNU General Public License
 version 3 can be found in /usr/share/common-licenses/GPL-3.
EOF
chmod 0644 "$PACKAGE_ROOT/usr/share/doc/cmux/copyright"
printf '%s\n' "$VERSION" > "$PACKAGE_ROOT/usr/share/doc/cmux/version"
chmod 0644 "$PACKAGE_ROOT/usr/share/doc/cmux/version"
cat > "$PACKAGE_ROOT/usr/share/doc/cmux/changelog" <<EOF
cmux (${VERSION}) unstable; urgency=medium

  * Add initial Ubuntu 26.04 GTK desktop package.

 -- Manaflow <hello@cmux.com>  Tue, 28 Apr 2026 00:00:00 +0000
EOF
gzip -9n "$PACKAGE_ROOT/usr/share/doc/cmux/changelog"
cat > "$PACKAGE_ROOT/usr/share/man/man1/cmux.1" <<'EOF'
.TH CMUX 1 "April 2026" "cmux" "User Commands"
.SH NAME
cmux \- terminal workspace for AI coding agents
.SH SYNOPSIS
\fBcmux\fR [\fB--version\fR]
.PP
\fBcmux app\fR
.PP
\fBcmux\fR \fICOMMAND\fR [\fIARGS\fR...]
.SH DESCRIPTION
cmux launches the Linux desktop shell with \fBcmux app\fR and relays other
commands to the running cmux app through the local socket.
.SH COMMON COMMANDS
.TP
.B cmux browser open URL
Open a browser surface in a new split.
.TP
.B cmux browser wait --selector SELECTOR
Wait for a browser condition.
.TP
.B cmux browser click --selector SELECTOR
Click a browser element.
.TP
.B cmux send --text TEXT
Send text to the active terminal surface.
.SH OPTIONS
.TP
.B --version
Print the installed cmux package version.
EOF
gzip -9n "$PACKAGE_ROOT/usr/share/man/man1/cmux.1"
cat > "$PACKAGE_ROOT/usr/share/lintian/overrides/cmux" <<'EOF'
cmux: statically-linked-binary [usr/lib/cmux/cmuxd-remote]
cmux: groff-message * [usr/share/man/man1/cmux.1.gz:*]
EOF
chmod 0644 "$PACKAGE_ROOT/usr/share/lintian/overrides/cmux"

find "$PACKAGE_ROOT" -type d -exec chmod 0755 {} +
find "$PACKAGE_ROOT" -type f -exec chmod go-w {} +
chmod 0755 "$PACKAGE_ROOT/usr/bin/cmux" "$PACKAGE_ROOT/usr/lib/cmux/cmux-gui" "$PACKAGE_ROOT/usr/lib/cmux/cmuxd-remote"
if [[ -f "$PACKAGE_ROOT/usr/lib/cmux/libghostty-gtk-embed.so" ]]; then
  chmod 0755 "$PACKAGE_ROOT/usr/lib/cmux/libghostty-gtk-embed.so"
fi
(
  cd "$PACKAGE_ROOT"
  find usr -type f -print0 |
    sort -z |
    xargs -0 md5sum > DEBIAN/md5sums
)

install -d -m 0755 "$SHLIBDEPS_DIR/debian"
cat > "$SHLIBDEPS_DIR/debian/control" <<'EOF'
Source: cmux
Section: devel
Priority: optional
Maintainer: Manaflow <hello@cmux.com>
Standards-Version: 4.7.0

Package: cmux
Architecture: any
Depends: ${shlibs:Depends}
Description: cmux
 cmux
EOF
SHLIB_INPUTS=(-e"$PACKAGE_ROOT/usr/lib/cmux/cmux-gui")
if [[ -f "$PACKAGE_ROOT/usr/lib/cmux/libghostty-gtk-embed.so" ]]; then
  SHLIB_INPUTS+=(-e"$PACKAGE_ROOT/usr/lib/cmux/libghostty-gtk-embed.so")
fi
DEB_DEPENDS="$(
  cd "$SHLIBDEPS_DIR"
  dpkg-shlibdeps -O "${SHLIB_INPUTS[@]}" 2>/dev/null |
    sed -n 's/^shlibs:Depends=//p'
)"
if [[ -z "$DEB_DEPENDS" ]]; then
  echo "error: failed to derive shared library dependencies" >&2
  exit 1
fi
DEB_DEPENDS="${DEB_DEPENDS}, libvte-2.91-gtk4-0"

INSTALLED_SIZE="$(du -sk "$PACKAGE_ROOT/usr" | awk '{print $1}')"
cat > "$PACKAGE_ROOT/DEBIAN/control" <<EOF
Package: cmux
Version: ${VERSION}
Section: devel
Priority: optional
Architecture: ${ARCH}
Maintainer: Manaflow <hello@cmux.com>
Installed-Size: ${INSTALLED_SIZE}
Depends: ${DEB_DEPENDS}
Homepage: https://github.com/manaflow-ai/cmux
Description: Terminal workspace for AI coding agents
 cmux is a terminal workspace for running coding agents in parallel.
 This Ubuntu package contains the Linux GTK desktop shell and CLI bridge.
EOF

desktop-file-validate "$PACKAGE_ROOT/usr/share/applications/com.cmuxterm.cmux.desktop"
appstreamcli validate --no-net "$PACKAGE_ROOT/usr/share/metainfo/com.cmuxterm.cmux.metainfo.xml"

PACKAGE_PATH="$OUTPUT_DIR/cmux_${VERSION}_${ARCH}.deb"
dpkg-deb --build --root-owner-group "$PACKAGE_ROOT" "$PACKAGE_PATH"

echo "Built $PACKAGE_PATH"
