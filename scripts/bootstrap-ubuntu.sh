#!/usr/bin/env bash
# One-time setup of build dependencies for OFM on Ubuntu (24.04 LTS or newer).
set -euo pipefail

sudo apt-get update
sudo apt-get install -y \
  build-essential pkg-config git curl \
  libgtk-4-dev libadwaita-1-dev libvte-2.91-gtk4-dev \
  liblzma-dev libbz2-dev libzstd-dev \
  xvfb xauth imagemagick \
  desktop-file-utils appstream \
  zsh fish

if ! command -v cargo >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
fi
rustup update stable
rustup component add rustfmt clippy

echo
echo "Versions:"
pkg-config --modversion gtk4 libadwaita-1 vte-2.91-gtk4 | paste -sd' ' | sed 's/^/  gtk4 libadwaita vte: /'
cargo --version
echo "Done. Next: git init -b main && git add -A && git commit -m 'chore: starter kit' && claude"
