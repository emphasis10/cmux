#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
DIST_DIR="$PROJECT_DIR/dist/linux"
DEB_DIR="$PROJECT_DIR/dist/deb"
VERSION="${CMUX_LINUX_DEB_VERSION:-0.1.0}"
BACKEND="${CMUX_E2E_BACKEND:-all}"
INSTALL_DEPS=1
BUILD_PACKAGE=1
INSTALL_DEB=1
RUN_SMOKE=1
KEEP_RUNNING=0
ZIG_VERSION="${CMUX_ZIG_VERSION:-0.15.2}"
GHOSTTY_FLAGS="${CMUX_GHOSTTY_GTK_EMBED_BUILD_FLAGS:-}"
CURRENT_APP_PID=""
CURRENT_RUN_DIR=""
CURRENT_KEEP_DIR=0
CMUX_E2E_LOG=""

usage() {
  cat <<EOF
Usage: $0 [options]

Build, install, and smoke-test the Linux cmux package.

Options:
  --version <version>       Debian package version. Default: $VERSION
  --backend <name>          pty, ghostty, auto, or all. Default: $BACKEND
  --ghostty-flags <flags>   Extra Zig flags for Ghostty, e.g. '-Dgtk-wayland=false -Dgtk-x11=true'
  --skip-deps              Do not install apt build dependencies
  --skip-build             Do not build/package; use existing artifacts or installed package
  --no-install             Build/package only; run smoke test against dist artifacts
  --skip-smoke             Do not launch cmux or run socket smoke checks
  --keep-running           Leave smoke-test cmux apps running
  -h, --help               Show this help

Environment:
  CMUX_LINUX_DEB_VERSION
  CMUX_E2E_BACKEND
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

cleanup_current() {
  local status="${1:-0}"
  if [[ "$status" -ne 0 && -n "${CMUX_E2E_LOG:-}" && -r "$CMUX_E2E_LOG" ]]; then
    log "cmux app log ($CMUX_E2E_LOG)"
    sed -n '1,220p' "$CMUX_E2E_LOG" >&2 || true
  fi
  if [[ -n "$CURRENT_APP_PID" ]]; then
    kill "$CURRENT_APP_PID" >/dev/null 2>&1 || true
    wait "$CURRENT_APP_PID" >/dev/null 2>&1 || true
    CURRENT_APP_PID=""
  fi
  if [[ -n "$CURRENT_RUN_DIR" ]]; then
    if [[ "$CURRENT_KEEP_DIR" -eq 1 || "$status" -ne 0 ]]; then
      log "Kept runtime directory: $CURRENT_RUN_DIR"
    else
      rm -rf "$CURRENT_RUN_DIR"
    fi
    CURRENT_RUN_DIR=""
  fi
}

trap 'cleanup_current $?' EXIT

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      VERSION="${2:-}"
      [[ -n "$VERSION" ]] || die "--version requires a value"
      shift 2
      ;;
    --backend)
      BACKEND="${2:-}"
      [[ "$BACKEND" =~ ^(pty|ghostty|auto|all)$ ]] || die "--backend must be pty, ghostty, auto, or all"
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
    --skip-build)
      BUILD_PACKAGE=0
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
  [[ "$BUILD_PACKAGE" -eq 1 ]] || return 0

  log "Building Linux artifacts"
  if [[ -n "$GHOSTTY_FLAGS" ]]; then
    CMUX_GHOSTTY_GTK_EMBED_BUILD_FLAGS="$GHOSTTY_FLAGS" "$PROJECT_DIR/scripts/build-linux.sh"
  else
    "$PROJECT_DIR/scripts/build-linux.sh"
  fi

  [[ -x "$DIST_DIR/cmux-gui" ]] || die "missing $DIST_DIR/cmux-gui"
  [[ -x "$DIST_DIR/cmuxd-remote" ]] || die "missing $DIST_DIR/cmuxd-remote"

  log "Packaging Debian artifact"
  "$PROJECT_DIR/scripts/package-deb.sh" --version "$VERSION"

  local deb
  deb="$(find "$DEB_DIR" -name "cmux_${VERSION}_*.deb" -print -quit)"
  [[ -n "$deb" ]] || die "package not found in $DEB_DIR"
  local contents
  contents="$(mktemp "${TMPDIR:-/tmp}/cmux-deb-contents.XXXXXX")"
  dpkg-deb -c "$deb" > "$contents"
  grep -q '/usr/lib/cmux/cmux-gui' "$contents" ||
    die "package does not contain /usr/lib/cmux/cmux-gui"
  grep -q '/usr/lib/cmux/cmuxd-remote' "$contents" ||
    die "package does not contain /usr/lib/cmux/cmuxd-remote"
  rm -f "$contents"
}

latest_deb() {
  find "$DEB_DIR" -name "cmux_${VERSION}_*.deb" -print -quit
}

install_package() {
  [[ "$INSTALL_DEB" -eq 1 ]] || return 0
  require_tool sudo
  require_tool apt-get
  local deb
  deb="$(latest_deb)"
  [[ -n "$deb" ]] || die "no package found; run without --skip-build or use --no-install"
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

run_cli_json() {
  "${CLI_CMD[@]}" --socket "$CMUX_SOCKET_PATH" --json "$@"
}

wait_for_socket() {
  local pid="$1"
  for _ in $(seq 1 100); do
    if [[ -S "$CMUX_SOCKET_PATH" ]] && run_cli_json ping >/dev/null 2>&1; then
      return 0
    fi
    if ! kill -0 "$pid" >/dev/null 2>&1; then
      sed -n '1,160p' "$CMUX_E2E_LOG" >&2 || true
      die "cmux app exited before creating socket"
    fi
    sleep 0.25
  done
  die "socket was not created at $CMUX_SOCKET_PATH"
}

wait_for_terminal_backend() {
  local surface_id="$1"
  local expected="$2"
  local health=""
  for _ in $(seq 1 100); do
    health="$(run_cli_json surface-health --surface "$surface_id")"
    if [[ "$(printf '%s\n' "$health" | json_get backend)" == "$expected" ]]; then
      printf '%s\n' "$health"
      return 0
    fi
    sleep 0.25
  done
  printf '%s\n' "$health" >&2
  die "terminal surface did not mount with backend $expected"
}

expected_backend_for() {
  local backend="$1"
  local caps="$2"
  case "$backend" in
    pty) printf 'pty\n' ;;
    ghostty) printf 'ghostty\n' ;;
    auto)
      if [[ "$(printf '%s\n' "$caps" | json_get features.ghosttyRenderer)" == "true" ]]; then
        printf 'ghostty\n'
      else
        printf 'pty\n'
      fi
      ;;
  esac
}

run_cmux_suite() {
  local backend="$1"
  require_tool python3

  if [[ -z "${DISPLAY:-}" && -z "${WAYLAND_DISPLAY:-}" ]]; then
    log "Skipping $backend smoke test because DISPLAY/WAYLAND_DISPLAY is not set"
    return 0
  fi

  local run_dir settings state caps expected workspace workspace_id terminal surface_id
  local health sent read_result split split_pane_id browser browser_id browser_url notifications panes pane_surfaces
  run_dir="$(mktemp -d "${TMPDIR:-/tmp}/cmux-${backend}-e2e.XXXXXX")"
  CURRENT_RUN_DIR="$run_dir"
  CURRENT_KEEP_DIR="$KEEP_RUNNING"
  settings="$run_dir/settings.json"
  state="$run_dir/session-linux.json"
  CMUX_E2E_LOG="$run_dir/cmux-gui.log"

  printf '{"terminalBackend":"%s","desktopNotifications":false,"browserStateAutomation":true}\n' "$backend" > "$settings"
  mkdir -p "$run_dir/xdg-runtime"
  chmod 700 "$run_dir/xdg-runtime"

  if [[ "$INSTALL_DEB" -eq 1 && -x /usr/bin/cmux ]]; then
    APP_CMD=(/usr/bin/cmux app)
    CLI_CMD=(/usr/bin/cmux)
    if [[ -x /usr/lib/cmux/libghostty-gtk-embed.so ]]; then
      export CMUX_LIBGHOSTTY_GTK_EMBED_PATH="/usr/lib/cmux/libghostty-gtk-embed.so"
    fi
  else
    APP_CMD=("$DIST_DIR/cmux-gui")
    CLI_CMD=("$DIST_DIR/cmuxd-remote" cli)
    if [[ -x "$DIST_DIR/libghostty-gtk-embed.so" ]]; then
      export CMUX_LIBGHOSTTY_GTK_EMBED_PATH="$DIST_DIR/libghostty-gtk-embed.so"
    fi
  fi

  export CMUX_SOCKET_PATH="$run_dir/cmux.sock"
  export CMUX_SETTINGS_PATH="$settings"
  export CMUX_SESSION_STATE_PATH="$state"
  export CMUX_APP_ID_SUFFIX="e2e-${backend}-$$"
  export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-$run_dir/xdg-runtime}"

  log "Launching cmux GTK app for $backend suite"
  "${APP_CMD[@]}" >"$CMUX_E2E_LOG" 2>&1 &
  CURRENT_APP_PID="$!"
  wait_for_socket "$CURRENT_APP_PID"

  caps="$(run_cli_json capabilities)"
  printf '%s\n' "$caps" | json_assert 'obj["features"].get("socketControllable") is True' "socketControllable is not true"
  expected="$(expected_backend_for "$backend" "$caps")"
  if [[ "$backend" == "ghostty" ]]; then
    printf '%s\n' "$caps" | json_assert 'obj["features"].get("ghosttyLibrary") is True' "ghosttyLibrary is not true"
    printf '%s\n' "$caps" | json_assert 'obj["features"].get("ghosttyRenderer") is True' "ghosttyRenderer is not true"
  fi

  workspace="$(run_cli_json new-workspace --name "cmux-e2e-$backend")"
  workspace_id="$(printf '%s\n' "$workspace" | json_get workspace_id)"
  [[ -n "$workspace_id" ]] || die "workspace.create did not return workspace_id"

  run_cli_json current-workspace >/dev/null
  run_cli_json list-workspaces | json_assert 'len(obj.get("workspaces", [])) >= 1' "workspace.list returned no workspaces"

  terminal="$(run_cli_json new-surface --workspace "$workspace_id" --type terminal --command cat)"
  surface_id="$(printf '%s\n' "$terminal" | json_get surface_id)"
  [[ -n "$surface_id" ]] || die "surface.create did not return surface_id"
  health="$(wait_for_terminal_backend "$surface_id" "$expected")"
  printf '%s\n' "$health" | json_assert 'obj.get("live_io") is True' "terminal live_io is not true"

  sent="$(run_cli_json send --surface "$surface_id" --text "cmux-${backend}-terminal")"
  printf '%s\n' "$sent" | json_assert 'obj.get("live_io") is True' "send did not reach live terminal"
  run_cli_json send-key --surface "$surface_id" --key enter >/dev/null

  for _ in $(seq 1 80); do
    read_result="$(run_cli_json read-surface --surface "$surface_id")"
    if [[ "$(printf '%s\n' "$read_result" | json_get text)" == *"cmux-${backend}-terminal"* ]]; then
      break
    fi
    sleep 0.25
  done
  printf '%s\n' "$read_result" | json_assert '"cmux-" in obj.get("text", "") and "-terminal" in obj.get("text", "")' "terminal read_text did not include sent text"
  printf '%s\n' "$read_result" | json_assert 'obj.get("backend") in ("pty", "ghostty")' "terminal read_text backend missing"

  split="$(run_cli_json new-split --surface "$surface_id" --direction right)"
  split_pane_id="$(printf '%s\n' "$split" | json_get pane_id)"
  [[ -n "$split_pane_id" ]] || die "surface.split did not return pane_id"
  panes="$(run_cli_json list-panes --workspace "$workspace_id")"
  printf '%s\n' "$panes" | json_assert 'len(obj.get("panes", [])) >= 2' "pane.list did not include split pane"
  pane_surfaces="$(run_cli_json list-pane-surfaces --pane "$split_pane_id")"
  printf '%s\n' "$pane_surfaces" | json_assert 'len(obj.get("surfaces", [])) >= 1' "pane.surfaces returned no surfaces"
  run_cli_json focus-pane --pane "$split_pane_id" >/dev/null
  run_cli_json resize-pane --pane "$split_pane_id" --ratio 0.55 >/dev/null
  run_cli_json equalize-splits --workspace "$workspace_id" >/dev/null
  run_cli_json last-pane >/dev/null

  browser="$(run_cli_json browser open --workspace "$workspace_id" --url "https://example.com")"
  browser_id="$(printf '%s\n' "$browser" | json_get surface_id)"
  [[ -n "$browser_id" ]] || die "browser open did not return surface_id"
  browser_url="$(run_cli_json browser get-url --surface "$browser_id")"
  printf '%s\n' "$browser_url" | json_assert 'obj.get("url") == "https://example.com"' "browser get-url mismatch"
  run_cli_json browser snapshot --surface "$browser_id" >/dev/null

  run_cli_json notify --title "cmux e2e" --body "backend $backend" --workspace "$workspace_id" >/dev/null
  notifications="$(run_cli_json rpc notification.list '{}')"
  printf '%s\n' "$notifications" | json_assert 'len(obj.get("notifications", [])) >= 1' "notification.list returned no notifications"
  run_cli_json rpc notification.clear '{}' >/dev/null

  run_cli_json close-surface --surface "$browser_id" >/dev/null
  run_cli_json close-surface --surface "$surface_id" >/dev/null

  log "cmux $backend suite passed"
  cleanup_current 0
}

selected_backends() {
  case "$BACKEND" in
    all)
      printf 'pty\n'
      if [[ -x "$DIST_DIR/libghostty-gtk-embed.so" || -x /usr/lib/cmux/libghostty-gtk-embed.so ]]; then
        printf 'ghostty\n'
      else
        log "Skipping ghostty backend because libghostty-gtk-embed.so was not found"
      fi
      ;;
    *) printf '%s\n' "$BACKEND" ;;
  esac
}

main() {
  cd "$PROJECT_DIR"
  install_deps
  if [[ "$BUILD_PACKAGE" -eq 1 ]]; then
    ensure_zig
  fi
  build_and_package
  install_package
  if [[ "$RUN_SMOKE" -eq 1 ]]; then
    while IFS= read -r backend; do
      [[ -n "$backend" ]] || continue
      run_cmux_suite "$backend"
    done < <(selected_backends)
  fi
  log "Done"
}

main "$@"
