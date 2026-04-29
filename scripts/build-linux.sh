#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
DIST_DIR="${PROJECT_DIR}/dist/linux"

mkdir -p "$DIST_DIR"

for tool in cargo go pkg-config; do
  command -v "$tool" >/dev/null || {
    echo "error: missing required tool: $tool" >&2
    exit 1
  }
done

STRIP_TOOL="${STRIP:-strip}"

if [[ -z "${CMUX_SKIP_GHOSTTY_GTK_EMBED_BUILD:-}" && -d "$PROJECT_DIR/ghostty" ]]; then
  if command -v zig >/dev/null 2>&1; then
    echo "==> Building Ghostty GTK embed library"
    (
      cd "$PROJECT_DIR/ghostty"
      read -r -a GHOSTTY_GTK_EMBED_BUILD_FLAGS <<< "${CMUX_GHOSTTY_GTK_EMBED_BUILD_FLAGS:-}"
      zig build gtk-embed-lib -Dapp-runtime=gtk -Doptimize=ReleaseFast "${GHOSTTY_GTK_EMBED_BUILD_FLAGS[@]}"
    )
  else
    echo "==> zig not found; skipping Ghostty GTK embed library build"
  fi
fi

echo "==> Building cmux GTK app"
cargo build \
  --manifest-path "$PROJECT_DIR/linux/cmux-gtk/Cargo.toml" \
  --release

install -m 0755 \
  "$PROJECT_DIR/linux/cmux-gtk/target/release/cmux-gui" \
  "$DIST_DIR/cmux-gui"
if command -v "$STRIP_TOOL" >/dev/null 2>&1; then
  "$STRIP_TOOL" --strip-unneeded "$DIST_DIR/cmux-gui" || true
fi

GHOSTTY_GTK_EMBED_LIB=""
if [[ -n "${CMUX_LIBGHOSTTY_GTK_EMBED_PATH:-}" && -f "$CMUX_LIBGHOSTTY_GTK_EMBED_PATH" ]]; then
  GHOSTTY_GTK_EMBED_LIB="$CMUX_LIBGHOSTTY_GTK_EMBED_PATH"
elif [[ -f "$PROJECT_DIR/ghostty/zig-out/lib/libghostty-gtk-embed.so" ]]; then
  GHOSTTY_GTK_EMBED_LIB="$PROJECT_DIR/ghostty/zig-out/lib/libghostty-gtk-embed.so"
elif [[ -f "$PROJECT_DIR/dist/linux/libghostty-gtk-embed.so" ]]; then
  GHOSTTY_GTK_EMBED_LIB="$PROJECT_DIR/dist/linux/libghostty-gtk-embed.so"
fi

if [[ -n "$GHOSTTY_GTK_EMBED_LIB" ]]; then
  echo "==> Including libghostty-gtk-embed"
  if [[ "$GHOSTTY_GTK_EMBED_LIB" != "$DIST_DIR/libghostty-gtk-embed.so" ]]; then
    install -m 0755 "$GHOSTTY_GTK_EMBED_LIB" "$DIST_DIR/libghostty-gtk-embed.so"
  fi
  if command -v "$STRIP_TOOL" >/dev/null 2>&1; then
    "$STRIP_TOOL" --strip-unneeded "$DIST_DIR/libghostty-gtk-embed.so" || true
  fi
else
  echo "==> libghostty-gtk-embed not found; Linux build will use fallback terminal backends"
fi

echo "==> Building cmuxd-remote for host Linux"
(
  cd "$PROJECT_DIR/daemon/remote"
  GOOS=linux \
  CGO_ENABLED=0 \
  go build -trimpath -buildvcs=false -ldflags "-s -w" \
    -o "$DIST_DIR/cmuxd-remote" \
    ./cmd/cmuxd-remote
)
if command -v "$STRIP_TOOL" >/dev/null 2>&1; then
  "$STRIP_TOOL" --strip-unneeded "$DIST_DIR/cmuxd-remote" || true
fi

echo "==> Linux build artifacts:"
printf '  %s\n' "$DIST_DIR/cmux-gui" "$DIST_DIR/cmuxd-remote"
if [[ -f "$DIST_DIR/libghostty-gtk-embed.so" ]]; then
  printf '  %s\n' "$DIST_DIR/libghostty-gtk-embed.so"
fi
