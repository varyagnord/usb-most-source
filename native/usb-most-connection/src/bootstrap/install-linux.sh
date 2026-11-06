#!/bin/sh
# Fixed bounded payload decoder. No stdin is sourced/eval'd, including sudo data.
set -eu
umask 077
IFS= read -r magic || exit 70
# sudo -S may leave the password unread under NOPASSWD. Treat it only as data.
if [ "$magic" != UMBOOT1 ]; then IFS= read -r magic || exit 70; fi
[ "$magic" = UMBOOT1 ] || exit 70
[ "$(id -u)" -eq 0 ] || exit 77
IFS= read -r expected || exit 70
# A password equal to the marker is data too, not a second transaction.
if [ "$expected" = UMBOOT1 ]; then IFS= read -r expected || exit 70; fi
[ "${#expected}" -eq 64 ] || exit 70
case "$expected" in *[!0-9a-f]*) exit 70 ;; esac
IFS= read -r binary || exit 70
[ "${#binary}" -le 33554432 ] || exit 70
IFS= read -r config || exit 70
[ "${#config}" -le 8192 ] || exit 70
# No extra data or concatenated transaction is permitted.
if IFS= read -r extra; then exit 70; fi
stage=$(mktemp -d /var/tmp/usb-most.XXXXXXXX) || exit 73
cleanup() {
  rm -f -- "$stage/receiver" "$stage/config"
  rmdir -- "$stage" 2>/dev/null || :
}
trap cleanup EXIT HUP INT TERM
printf '%s' "$binary" | base64 -d > "$stage/receiver" || exit 70
unset binary
actual=$(sha256sum "$stage/receiver")
actual=${actual%% *}
[ "$actual" = "$expected" ] || exit 74
printf '%s' "$config" | base64 -d > "$stage/config" || exit 70
unset config
chmod 0700 "$stage/receiver"
"$stage/receiver" --install-stdin < "$stage/config"
