# OFM - Orthodox File Manager

A keyboard-first, two-panel file manager for Ubuntu GNOME, in the tradition of Norton Commander,
Far Manager, Midnight Commander and Total Commander. Written in Rust with GTK 4 and libadwaita.

Status: pre-release (0.1.0). Try it on copies of your data first.

## Features

- Two panels with tabs, brief, full and custom views, selection by mask, quick search
- Command line and embedded terminal that follow the active panel
- Copy, move, delete (to Trash) and links as background jobs, with conflict handling
- Viewer with encodings and hex mode, quick view, external editor
- Find files, result panels, read-only archives (zip, tar, 7z), folder compare
- User menu, batch rename, rebindable keys (Far, Total Commander, mc), themes

## Install

Requirements: GTK 4.14, libadwaita 1.5 and VTE 0.76 or newer.

Build the package and install it:

```bash
cargo install cargo-deb      # once
cargo deb -p ofm-app
sudo apt install ./target/debian/ofm_*.deb
```

A package built on a new Ubuntu release may not install on an older one. Build it on the release
where you want to run it.

## Run from source

```bash
./scripts/bootstrap-ubuntu.sh        # once: system packages and Rust
cargo run --release -p ofm-app -- [LEFT_DIR [RIGHT_DIR]]
```

## Documentation

- [User guide](docs/USER-GUIDE.md): concepts, keys, themes, user menu, configuration, limitations
- `man ofm`
- `ofm --help`, `ofm --version`, `ofm --dump-keys far|tc|mc`
- [Changelog](CHANGELOG.md)

## Development

`./scripts/check.sh` runs formatting, clippy, all tests and a UI smoke test.
Design documents are in `docs/` (`SPEC.md`, `ARCHITECTURE.md`, `PLAN.md`, `adr/`).

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
