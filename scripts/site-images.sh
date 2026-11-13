#!/bin/bash
# SPDX-License-Identifier: GPL-2.0-only
#
# site-images.sh
#
# Renders the site's icons from packaging/site/icon.svg and its link
# preview from packaging/site/preview.html, into packaging/site. Run it
# after changing either and commit what it writes: the release build has
# none of these tools. Needs rsvg-convert, ImageMagick and Firefox.

set -euo pipefail
cd "$(dirname "$0")/../packaging/site"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

for size in 16 32 48; do
    rsvg-convert -w "$size" -h "$size" icon.svg -o "$tmp/$size.png"
done
magick "$tmp/16.png" "$tmp/32.png" "$tmp/48.png" favicon.ico
# Phones cut their own rounded corners and show transparent ones black, so
# this one is square, without the outline.
grep -v 'stroke=' icon.svg | rsvg-convert -w 180 -h 180 -b '#040805' -o apple-touch-icon.png

site=$(sed -n 's#^SITE_URL ?= https://##p' ../../Makefile)
sed "s#@SITE@#$site#" preview.html >"$tmp/preview.html"
cp -r fonts icon.svg "$tmp/"
mkdir "$tmp/profile"
firefox --headless --no-remote --profile "$tmp/profile" --window-size=1280,640 \
    --screenshot "$tmp/preview.png" "file://$tmp/preview.html" 2>/dev/null
magick "$tmp/preview.png" -strip -alpha off preview.png
file favicon.ico apple-touch-icon.png preview.png
