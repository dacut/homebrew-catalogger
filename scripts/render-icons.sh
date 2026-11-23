#!/usr/bin/env bash
#
# Renders the site's bitmap icons from crates/homebrew-catalog-static-pages/assets/favicon.svg:
#
#   favicon.ico           32x32, for browsers that request /favicon.ico or don't support SVG icons
#   apple-touch-icon.png  180x180 with an opaque, square background, for iOS home screens
#
# Run this after changing favicon.svg; the generator embeds the results. Requires Google Chrome (headless) and
# Python 3.

set -euo pipefail

# Renders favicon.svg at a size, on a background, to a PNG.
# Uses the globals work and chrome, set by main.
render() {
    local size=$1 background=$2 output=$3
    cat >"$work/render.html" <<EOF
<!doctype html>
<style>html, body { margin: 0; background: $background; } img { display: block; }</style>
<img src="favicon.svg" width="$size" height="$size" alt="">
EOF
    "$chrome" --headless=new --disable-gpu --hide-scrollbars --force-device-scale-factor=1 \
        --default-background-color=00000000 --user-data-dir="$work/profile" \
        --window-size="$size,$size" --screenshot="$output" "file://$work/render.html" >/dev/null 2>&1
}

# Writes an ICO file holding one 32x32 PNG image (supported by every browser since Internet Explorer 11).
write_ico() {
    local png=$1 ico=$2
    python3 -I - "$png" "$ico" <<'EOF'
import struct, sys
png = open(sys.argv[1], "rb").read()
header = struct.pack("<HHH", 0, 1, 1)
entry = struct.pack("<BBBBHHII", 32, 32, 0, 0, 1, 32, len(png), 6 + 16)
open(sys.argv[2], "wb").write(header + entry + png)
EOF
}

main() {
    repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
    assets="$repo_root/crates/homebrew-catalog-static-pages/assets"
    chrome=${CHROME:-"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"}
    [[ -x $chrome ]] || { echo "render-icons: Chrome not found; set CHROME to its executable" >&2; exit 1; }

    work=$(mktemp -d)
    trap 'rm -rf "$work"' EXIT
    cp "$assets/favicon.svg" "$work/favicon.svg"

    render 32 transparent "$work/favicon-32.png"
    # iOS rounds the corners itself and shows transparency as black, so fill the square with the icon's background.
    render 180 '#0550ae' "$assets/apple-touch-icon.png"

    write_ico "$work/favicon-32.png" "$assets/favicon.ico"

    echo "Rendered $assets/favicon.ico and $assets/apple-touch-icon.png"
}

# Everything runs inside main, which bash reads in full before running any of it, so editing this file while it runs
# can't change what runs. The exit on the same line stops bash from reading anything after it.
main "$@"; exit
