#!/bin/sh
# No executable is uploaded on a matching installation. Independently recheck
# the fixed path as root; an unprivileged probe is only a performance hint.
set -eu
umask 077
IFS= read -r magic || exit 70
if [ "$magic" != UMCONF1 ]; then IFS= read -r magic || exit 70; fi
[ "$magic" = UMCONF1 ] || exit 70
[ "$(id -u)" -eq 0 ] || exit 77
IFS= read -r expected || exit 70
if [ "$expected" = UMCONF1 ]; then IFS= read -r expected || exit 70; fi
[ "${#expected}" -eq 64 ] || exit 70
case "$expected" in *[!0-9a-f]*) exit 70 ;; esac
IFS= read -r config || exit 70
[ "${#config}" -le 8192 ] || exit 70
if IFS= read -r extra; then exit 70; fi
binary=/usr/local/libexec/usb-most/usb-most-desktop
for path in /usr /usr/local /usr/local/libexec /usr/local/libexec/usb-most "$binary"; do
  [ ! -L "$path" ] || exit 78
  [ "$(stat -c %u -- "$path" 2>/dev/null)" = 0 ] || exit 78
  mode=$(stat -c %a -- "$path") || exit 78
  [ "$((0$mode & 022))" -eq 0 ] || exit 78
done
[ -f "$binary" ] && [ -x "$binary" ] || exit 78
actual=$(sha256sum "$binary") || exit 78
actual=${actual%% *}
[ "$actual" = "$expected" ] || exit 78
# Keep decoder failure separate from a partially applied configuration. Only
# pre-mutation exit78 allows the caller to fall back to the full installer.
stage=$(mktemp -d /var/tmp/usb-most.XXXXXXXX) || exit 73
cleanup() {
  rm -f -- "$stage/config"
  rmdir -- "$stage" 2>/dev/null || :
}
trap cleanup EXIT HUP INT TERM
printf '%s' "$config" | base64 -d > "$stage/config" || exit 70
unset config
if "$binary" --install-stdin < "$stage/config"; then exit 0; else exit 79; fi
