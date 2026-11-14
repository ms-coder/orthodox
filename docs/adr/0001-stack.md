# ADR-0001: Language, toolkit and architecture style

Status: accepted · 2026-11-14

## Context

OFM is a keyboard-first two-panel file manager, GNOME-first, with later ports to other platforms
(other desktops, Windows/macOS, maybe a TUI). It must handle very large directories, never block the
UI, and never lose data in file operations.

## Decision

- **Rust** for everything (memory safety, threads without data races, good fs/syscall crates).
- **GTK 4 + libadwaita** via gtk4-rs for the GNOME frontend; **VTE (GTK 4)** for the embedded terminal.
- **Core/UI split:** all logic in UI-agnostic crates (`ofm-core`, `ofm-vfs`, `ofm-jobs`, `ofm-config`,
  `ofm-viewer`, `ofm-search`); `ofm-app` is a thin GTK frontend. Ports add new frontends instead of
  rewriting.
- **Threads + channels, no async runtime** in v1.0 (simpler; file I/O is blocking anyway).
- Minimum: GTK 4.14, libadwaita 1.5, VTE 0.76 (Ubuntu 24.04 LTS); develop on current Ubuntu.
- **.deb** packaging for v1.0; Flatpak later (a file manager with an embedded host shell needs broad
  sandbox exceptions, which deserves its own decision).

## Alternatives considered

- Qt 6 (C++ or cxx-qt): best cross-platform widgets, but less GNOME-native and clumsier from Rust.
- Slint / iced: promising, own look, less mature desktop integration (DnD, a11y, portals).
- Tauri/Electron: poor fit for huge lists and keyboard-heavy UI; WebKitGTK lags on Linux.
- Avalonia (C#): viable, but not GNOME-native.

## Consequences

- Windows/macOS later need a new frontend (Qt or Slint) on top of the same core.
- GTK specifics (GObject subclassing, main-loop integration) are confined to `ofm-app`.
