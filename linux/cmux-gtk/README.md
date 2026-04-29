# cmux GTK

This is the Ubuntu/Linux desktop shell for cmux. It is intentionally separate
from the SwiftUI/AppKit macOS app because the current application is deeply
tied to AppKit, WKWebView, Sparkle, and macOS GhosttyKit packaging.

Current status:

1. Starts a GTK4/libadwaita window.
2. Opens a Linux JSON-RPC Unix socket.
3. Reports Linux capabilities through `system.capabilities`.
4. Maintains in-memory workspace and surface records through early
   `workspace.*` and `surface.*` JSON-RPC methods.
5. Provides an interim PTY-backed shell panel and a WebKitGTK browser panel.
   The PTY panel is intentionally a bridge toward the planned libghostty
   terminal surface.

The target package is an Ubuntu 26.04 `.deb` installed as:

- `/usr/lib/cmux/cmux-gui`
- `/usr/lib/cmux/cmuxd-remote`
- `/usr/bin/cmux`
- `/usr/share/applications/com.cmuxterm.cmux.desktop`
- `/usr/share/metainfo/com.cmuxterm.cmux.metainfo.xml`
