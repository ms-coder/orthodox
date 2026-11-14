#!/usr/bin/env bash
# Create directories for manual testing (gate G1 and later).
# Usage: scripts/make-test-dirs.sh [TARGET]   (default /tmp/ofm-test)
set -euo pipefail
root="${1:-/tmp/ofm-test}"
mkdir -p "$root"

# 100k empty files for scrolling/sorting performance
if [[ ! -d "$root/100k" ]]; then
  mkdir -p "$root/100k"
  (cd "$root/100k" && seq -f "file_%06g.txt" 1 100000 | xargs touch)
fi

# Mixed directory with tricky names and file types
m="$root/mixed"
mkdir -p "$m/dir with spaces" "$m/Кириллица" "$m/.hidden-dir"
printf 'hello\n' > "$m/readme.txt"
printf 'Привет, мир\n' | iconv -f UTF-8 -t CP1251 > "$m/cp1251.txt"
printf 'Привет, мир\n' | iconv -f UTF-8 -t CP866  > "$m/cp866.txt"
printf 'Привет, мир\n' | iconv -f UTF-8 -t KOI8-R > "$m/koi8r.txt"
touch "$m/.hidden-file" "$m/file2" "$m/file10" "$m/File1" "$m/émoji-😀.txt"
touch "$m/$(printf 'bad-utf8-\xff\xfe')" 2>/dev/null || true
truncate -s 2G "$m/sparse-2g.bin"
head -c 1048576 /dev/urandom > "$m/random-1m.bin"
printf '#!/bin/sh\necho hi from script\n' > "$m/script.sh" && chmod +x "$m/script.sh"
ln -sfn readme.txt "$m/link-to-readme"
ln -sfn does-not-exist "$m/broken-link"
ln -sfn "$m" "$m/loop-link"

# Deep nesting
d="$m/deep"
for i in $(seq 1 50); do d="$d/level$i"; done
mkdir -p "$d" && touch "$d/bottom.txt"

# Archives (if tools exist)
(cd "$m" && command -v zip >/dev/null && zip -qr ../sample.zip readme.txt "dir with spaces" || true)
(cd "$m" && tar czf ../sample.tar.gz readme.txt cp1251.txt)

echo "Created test dirs under $root"
