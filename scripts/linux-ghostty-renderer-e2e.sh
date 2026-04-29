#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
DIST_DIR="$PROJECT_DIR/dist/linux"
DEB_DIR="$PROJECT_DIR/dist/deb"
VERSION="${CMUX_LINUX_DEB_VERSION:-0.1.0}"
INSTALL_DEPS=1
INSTALL_DEB=1
RUN_SMOKE=1
KEEP_RUNNING=0
ZIG_VERSION="${CMUX_ZIG_VERSION:-0.15.2}"
GHOSTTY_FLAGS="${CMUX_GHOSTTY_GTK_EMBED_BUILD_FLAGS:-}"

usage() {
  cat <<EOF
Usage: $0 [options]

Build, package, optionally install, and smoke-test the Linux Ghostty renderer.

Options:
  --version <version>       Debian package version. Default: $VERSION
  --ghostty-flags <flags>   Extra Zig flags for Ghostty, e.g. '-Dgtk-wayland=false -Dgtk-x11=true'
  --skip-deps              Do not install apt build dependencies
  --no-install             Build/package only; do not install the .deb
  --skip-smoke             Do not launch cmux or run socket smoke checks
  --keep-running           Leave the smoke-test cmux app running
  -h, --help               Show this help

Environment:
  CMUX_LINUX_DEB_VERSION
  CMUX_GHOSTTY_GTK_EMBED_BUILD_FLAGS
  CMUX_ZIG_VERSION
EOF
}

log() {
  printf '==> %s\n' "$*" >&2
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      VERSION="${2:-}"
      [[ -n "$VERSION" ]] || die "--version requires a value"
      shift 2
      ;;
    --ghostty-flags)
      GHOSTTY_FLAGS="${2:-}"
      shift 2
      ;;
    --skip-deps)
      INSTALL_DEPS=0
      shift
      ;;
    --no-install)
      INSTALL_DEB=0
      shift
      ;;
    --skip-smoke)
      RUN_SMOKE=0
      shift
      ;;
    --keep-running)
      KEEP_RUNNING=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "unknown option: $1"
      ;;
  esac
done

require_tool() {
  command -v "$1" >/dev/null 2>&1 || die "missing required tool: $1"
}

install_deps() {
  [[ "$INSTALL_DEPS" -eq 1 ]] || return 0
  require_tool sudo
  require_tool apt-get
  log "Installing Ubuntu build dependencies"
  sudo apt-get update
  sudo apt-get install -y \
    build-essential cargo rustc golang-go pkg-config \
    libgtk-4-dev libadwaita-1-dev libwebkitgtk-6.0-dev \
    libvte-2.91-gtk4-0 libgtk4-layer-shell-dev \
    blueprint-compiler libxml2-utils xz-utils curl python3 \
    dpkg-dev desktop-file-utils appstream lintian
}

ensure_zig() {
  if command -v zig >/dev/null 2>&1; then
    log "Using Zig: $(command -v zig)"
    return 0
  fi

  require_tool curl
  require_tool tar

  local machine zig_arch zig_dir tarball url
  machine="$(uname -m)"
  case "$machine" in
    x86_64|amd64) zig_arch="x86_64" ;;
    aarch64|arm64) zig_arch="aarch64" ;;
    *) die "unsupported architecture for Zig bootstrap: $machine" ;;
  esac

  zig_dir="/tmp/cmux-zig/zig-${zig_arch}-linux-${ZIG_VERSION}"
  tarball="/tmp/cmux-zig/zig-${zig_arch}-linux-${ZIG_VERSION}.tar.xz"
  url="https://ziglang.org/download/${ZIG_VERSION}/zig-${zig_arch}-linux-${ZIG_VERSION}.tar.xz"

  if [[ ! -x "$zig_dir/zig" ]]; then
    log "Downloading Zig $ZIG_VERSION"
    mkdir -p /tmp/cmux-zig
    curl -fsSL "$url" -o "$tarball"
    tar -C /tmp/cmux-zig -xf "$tarball"
  fi

  export PATH="$zig_dir:$PATH"
  log "Using Zig: $zig_dir/zig"
}

build_and_package() {
  log "Building Linux artifacts"
  if [[ -n "$GHOSTTY_FLAGS" ]]; then
    CMUX_GHOSTTY_GTK_EMBED_BUILD_FLAGS="$GHOSTTY_FLAGS" "$PROJECT_DIR/scripts/build-linux.sh"
  else
    "$PROJECT_DIR/scripts/build-linux.sh"
  fi

  [[ -x "$DIST_DIR/cmux-gui" ]] || die "missing $DIST_DIR/cmux-gui"
  [[ -x "$DIST_DIR/cmuxd-remote" ]] || die "missing $DIST_DIR/cmuxd-remote"
  [[ -x "$DIST_DIR/libghostty-gtk-embed.so" ]] || die "missing $DIST_DIR/libghostty-gtk-embed.so"

  log "Packaging Debian artifact"
  "$PROJECT_DIR/scripts/package-deb.sh" --version "$VERSION"

  local deb
  deb="$(find "$DEB_DIR" -name "cmux_${VERSION}_*.deb" -print -quit)"
  [[ -n "$deb" ]] || die "package not found in $DEB_DIR"
  local contents
  contents="$(mktemp "${TMPDIR:-/tmp}/cmux-deb-contents.XXXXXX")"
  dpkg-deb -c "$deb" > "$contents"
  grep -q '/usr/lib/cmux/libghostty-gtk-embed.so' "$contents" ||
    die "package does not contain /usr/lib/cmux/libghostty-gtk-embed.so"
  rm -f "$contents"
  printf '%s\n' "$deb"
}

install_package() {
  local deb="$1"
  [[ "$INSTALL_DEB" -eq 1 ]] || return 0
  require_tool sudo
  require_tool apt-get
  log "Installing $deb"
  sudo apt-get install -y "$deb"
}

json_get() {
  local path="$1"
  python3 -c '
import json, sys
obj = json.load(sys.stdin)
for part in sys.argv[1].split("."):
    if isinstance(obj, dict):
        obj = obj.get(part)
    else:
        obj = None
        break
if isinstance(obj, bool):
    print("true" if obj else "false")
elif obj is None:
    print("")
else:
    print(obj)
' "$path"
}

json_assert() {
  local expr="$1"
  local message="$2"
  python3 -c '
import json, sys
obj = json.load(sys.stdin)
if not eval(sys.argv[1], {"__builtins__": {}}, {"obj": obj}):
    raise SystemExit(sys.argv[2])
' "$expr" "$message"
}

run_smoke() {
  [[ "$RUN_SMOKE" -eq 1 ]] || return 0
  require_tool python3

  if [[ -z "${DISPLAY:-}" && -z "${WAYLAND_DISPLAY:-}" ]]; then
    log "Skipping runtime smoke test because DISPLAY/WAYLAND_DISPLAY is not set"
    return 0
  fi

  local run_dir socket settings state log_file app_pid
  run_dir="$(mktemp -d "${TMPDIR:-/tmp}/cmux-ghostty-smoke.XXXXXX")"
  socket="$run_dir/cmux.sock"
  settings="$run_dir/settings.json"
  state="$run_dir/session-linux.json"
  log_file="$run_dir/cmux-gui.log"
  app_pid=""

  cleanup() {
    if [[ -n "$app_pid" && "$KEEP_RUNNING" -eq 0 ]]; then
      kill "$app_pid" >/dev/null 2>&1 || true
      wait "$app_pid" >/dev/null 2>&1 || true
    fi
    if [[ "$KEEP_RUNNING" -eq 0 ]]; then
      rm -rf "$run_dir"
    else
      log "Kept runtime directory: $run_dir"
    fi
  }
  trap cleanup RETURN

  printf '{"terminalBackend":"ghostty","desktopNotifications":false}\n' > "$settings"
  mkdir -p "$run_dir/xdg-runtime"
  chmod 700 "$run_dir/xdg-runtime"

  local -a app_cmd cli_cmd
  if [[ "$INSTALL_DEB" -eq 1 && -x /usr/bin/cmux ]]; then
    app_cmd=(/usr/bin/cmux app)
    cli_cmd=(/usr/bin/cmux)
    export CMUX_LIBGHOSTTY_GTK_EMBED_PATH="/usr/lib/cmux/libghostty-gtk-embed.so"
  else
    app_cmd=("$DIST_DIR/cmux-gui")
    cli_cmd=("$DIST_DIR/cmuxd-remote" cli)
    export CMUX_LIBGHOSTTY_GTK_EMBED_PATH="$DIST_DIR/libghostty-gtk-embed.so"
  fi

  export CMUX_SOCKET_PATH="$socket"
  export CMUX_SETTINGS_PATH="$settings"
  export CMUX_SESSION_STATE_PATH="$state"
  export CMUX_APP_ID_SUFFIX="ghostty-smoke-$$"
  export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-$run_dir/xdg-runtime}"

  log "Launching cmux GTK app for smoke test"
  "${app_cmd[@]}" >"$log_file" 2>&1 &
  app_pid="$!"

  for _ in $(seq 1 80); do
    if [[ -S "$socket" ]] && "${cli_cmd[@]}" --socket "$socket" ping >/dev/null 2>&1; then
      break
    fi
    if ! kill -0 "$app_pid" >/dev/null 2>&1; then
      sed -n '1,120p' "$log_file" >&2 || true
      die "cmux app exited before creating socket"
    fi
    sleep 0.25
  done
  [[ -S "$socket" ]] || die "socket was not created at $socket"

  local caps workspace workspace_id surface surface_id read_result health send_result
  caps="$("${cli_cmd[@]}" --socket "$socket" --json capabilities)"
  printf '%s\n' "$caps" | json_assert 'obj["features"].get("ghosttyLibrary") is True' "ghosttyLibrary is not true"
  printf '%s\n' "$caps" | json_assert 'obj["features"].get("ghosttyRenderer") is True' "ghosttyRenderer is not true"

  workspace="$("${cli_cmd[@]}" --socket "$socket" --json new-workspace --name GhosttySmoke)"
  workspace_id="$(printf '%s\n' "$workspace" | json_get workspace_id)"
  [[ -n "$workspace_id" ]] || die "workspace.create did not return workspace_id"

  surface="$("${cli_cmd[@]}" --socket "$socket" --json new-surface \
    --workspace "$workspace_id" \
    --type terminal \
    --command cat)"
  surface_id="$(printf '%s\n' "$surface" | json_get surface_id)"
  [[ -n "$surface_id" ]] || die "surface.create did not return surface_id"

  for _ in $(seq 1 80); do
    health="$("${cli_cmd[@]}" --socket "$socket" --json surface-health --surface "$surface_id")"
    if [[ "$(printf '%s\n' "$health" | json_get backend)" == "ghostty" ]]; then
      break
    fi
    sleep 0.25
  done
  printf '%s\n' "$health" | json_assert 'obj.get("backend") == "ghostty"' "surface backend is not ghostty"
  printf '%s\n' "$health" | json_assert 'obj.get("live_io") is True' "surface live_io is not true"

  send_result="$("${cli_cmd[@]}" --socket "$socket" --json send --surface "$surface_id" --text "cmux-ghostty-input")"
  printf '%s\n' "$send_result" | json_assert 'obj.get("live_io") is True' "send_text did not reach live terminal"
  "${cli_cmd[@]}" --socket "$socket" --json send-key --surface "$surface_id" --key enter >/dev/null

  for _ in $(seq 1 80); do
    read_result="$("${cli_cmd[@]}" --socket "$socket" --json read-surface --surface "$surface_id")"
    if [[ "$(printf '%s\n' "$read_result" | json_get text)" == *"cmux-ghostty-input"* ]]; then
      break
    fi
    sleep 0.25
  done
  printf '%s\n' "$read_result" | json_assert '"cmux-ghostty-input" in obj.get("text", "")' "read_text did not include sent input"
  printf '%s\n' "$read_result" | json_assert 'obj.get("backend") == "ghostty"' "read_text backend is not ghostty"

  "${cli_cmd[@]}" --socket "$socket" --json close-surface --surface "$surface_id" >/dev/null
  log "Ghostty renderer smoke test passed"
}

main() {
  cd "$PROJECT_DIR"
  install_deps
  ensure_zig
  local deb
  deb="$(build_and_package)"
  install_package "$deb"
  run_smoke
  log "Done"
}

main "$@"
