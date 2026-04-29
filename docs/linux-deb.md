# Ubuntu Debian Package

cmux now has an additive Linux port scaffold under `linux/cmux-gtk`. The
macOS app remains the production app; the Linux package is the start of the
Ubuntu 26.04 desktop implementation.

## Build

Install Ubuntu 26.04 build dependencies:

```bash
sudo apt-get update
sudo apt-get install -y \
  build-essential cargo rustc golang-go pkg-config \
  libgtk-4-dev libadwaita-1-dev libwebkitgtk-6.0-dev \
  libvte-2.91-gtk4-0 libgtk4-layer-shell-dev \
  blueprint-compiler libxml2-utils \
  dpkg-dev desktop-file-utils appstream lintian
```

The Ghostty renderer build also requires the Zig version declared by
`ghostty/build.zig.zon`.

Build the Linux binaries:

```bash
./scripts/build-linux.sh
```

The build script also builds the Linux Ghostty GTK embed library when `zig` and
the `ghostty` submodule are available:

```bash
cd ghostty
zig build gtk-embed-lib -Dapp-runtime=gtk -Doptimize=ReleaseFast
```

Set `CMUX_SKIP_GHOSTTY_GTK_EMBED_BUILD=1` to skip that step, or set
`CMUX_LIBGHOSTTY_GTK_EMBED_PATH=/path/to/libghostty-gtk-embed.so` to use a
prebuilt library. Additional Zig flags can be passed with
`CMUX_GHOSTTY_GTK_EMBED_BUILD_FLAGS`, for example
`CMUX_GHOSTTY_GTK_EMBED_BUILD_FLAGS="-Dgtk-wayland=false -Dgtk-x11=true"` on
minimal build hosts that do not have `libgtk4-layer-shell-dev`. The script also picks up
`ghostty/zig-out/lib/libghostty-gtk-embed.so`. When present, the library is
copied to `dist/linux/libghostty-gtk-embed.so` and included in the Debian
package at `/usr/lib/cmux/libghostty-gtk-embed.so`.

Create a `.deb`:

```bash
./scripts/package-deb.sh --version 0.1.0
```

Create an unsigned local APT repository:

```bash
./scripts/package-apt-repo.sh
```

The package installs:

- `/usr/bin/cmux`
- `/usr/lib/cmux/cmux-gui`
- `/usr/lib/cmux/cmuxd-remote`
- `/usr/lib/cmux/libghostty-gtk-embed.so` when the optional Ghostty renderer exists
- `/usr/share/applications/com.cmuxterm.cmux.desktop`
- `/usr/share/metainfo/com.cmuxterm.cmux.metainfo.xml`

For isolated local smoke runs while another cmux app is already registered on
the desktop bus, set `CMUX_APP_ID_SUFFIX=<name>` together with a temporary
`CMUX_SOCKET_PATH`.

Runtime settings are read from `$CMUX_SETTINGS_PATH`,
`$XDG_CONFIG_HOME/cmux/settings.json`, or `~/.config/cmux/settings.json`.
Supported Linux-specific keys include `sidebarWidth`, `terminalBackend`
(`pty`, `vte`, `ghostty`, or `auto`), `browserStateAutomation`, and
`desktopNotifications`.

## Current Feature State

Implemented:

- GTK4/libadwaita desktop shell.
- Linux Unix socket at `$CMUX_SOCKET_PATH` or `$XDG_RUNTIME_DIR/cmux/cmux.sock`.
- CLI discovery through `$CMUX_SOCKET_PATH`, the Linux default socket path, and
  `~/.cmux/socket_addr` written by the GTK app.
- JSON-RPC `ping`, `app.ping`, and `system.capabilities`.
- In-memory `workspace.list`, `workspace.create`, `surface.list`, and
  `surface.create` handlers for early CLI/automation integration.
- In-memory pane/surface model with compatibility handlers for common existing
  CLI commands: `cmux ping`, `list-workspaces`, `new-workspace`,
  `current-workspace`, `select-workspace`, `close-workspace`, `list-panels`,
  `new-pane`, `list-panes`, `list-pane-surfaces`, `new-surface`, `new-split`,
  `close-surface`, `focus-panel`, browser open, browser navigation/history,
  notification creation, and acknowledged terminal send-key/send-text calls.
- GTK sidebar refreshes when workspaces or surfaces are created through the
  socket.
- Debian package structure and CI hooks.
- Configurable terminal backend selection. `terminalBackend: "ghostty"` and
  `terminalBackend: "auto"` use the real in-process Ghostty GTK renderer when
  `libghostty-gtk-embed.so` loads and initializes. PTY/TextView remains the
  fallback backend and supports socket-driven live input, captured text, process
  health, and close-time cleanup. VTE can be selected with
  `terminalBackend: "vte"` for visual experimentation. Terminal surfaces
  created through `new-workspace`, `new-pane`, or `new-surface` honor
  `--command` and `--working-directory` on the Ghostty and PTY backends.
- Ghostty renderer probing through `CMUX_LIBGHOSTTY_GTK_EMBED_PATH`,
  `/usr/lib/cmux/libghostty-gtk-embed.so`,
  `/usr/local/lib/libghostty-gtk-embed.so`, and the dynamic linker. The Linux
  app reports `features.ghosttyLibrary` when the embed library is loadable and
  `features.ghosttyRenderer` only after all ABI symbols resolve and
  `ghostty_gtk_embed_init` succeeds.
- WebKitGTK browser panel with live navigate, focus, back, forward, reload,
  JavaScript evaluation, DOM snapshots, native PNG screenshots, common
  `browser.get.*` queries, waits, form/click interactions, selector lookup,
  visibility/enabled/checked predicates, DOM keyboard events, same-origin frame
  selection, console/error buffers, JavaScript dialog response, and download
  waiting. State-backed automation remains as a compatibility fallback for
  synthetic/data-url surfaces. The `cmux browser` CLI exposes common navigation
  and automation subcommands, including `wait`, `click`, `fill`, `type`,
  `press`, `screenshot`, `snapshot`, and `eval`.
- Desktop notification delivery through `GNotification` and an in-app
  notification list.
- Unsigned local APT repository layout generation from built `.deb` artifacts.

Still planned:

- Fill out full Ghostty GTK mouse event forwarding beyond GTK's native widget
  event handling.
- Fill out the remaining WebKit automation gaps: native input-event fidelity,
  cross-origin frame targeting, and richer per-resource/network events.
- Add signing and hosted publication for the APT repository after release
  infrastructure is ready.

## Ghostty Renderer Verification

Build and package:

```bash
./scripts/build-linux.sh
./scripts/package-deb.sh --version 0.1.0
dpkg-deb -c dist/deb/cmux_0.1.0_$(dpkg --print-architecture).deb | grep libghostty-gtk-embed
```

Runtime capability probe:

```bash
CMUX_SOCKET_PATH=/tmp/cmux-ghostty.sock cmux app
CMUX_SOCKET_PATH=/tmp/cmux-ghostty.sock cmux rpc system.capabilities
```

Expected capability fields when the renderer is usable:

```json
{
  "features": {
    "ghosttyLibrary": true,
    "ghosttyRenderer": true
  },
  "ghostty": {
    "rendererAvailable": true,
    "libraryPath": "/usr/lib/cmux/libghostty-gtk-embed.so"
  }
}
```

Fallback checks:

- Remove or rename `/usr/lib/cmux/libghostty-gtk-embed.so`; `auto` falls back to
  PTY and `features.ghosttyRenderer` is `false`.
- Set `terminalBackend: "pty"` in `~/.config/cmux/settings.json`; Ghostty is not
  used even if the library is installed.
- Set `CMUX_LIBGHOSTTY_GTK_EMBED_PATH=/bad/path`; capabilities include a clear
  unavailable reason and PTY fallback remains available.
