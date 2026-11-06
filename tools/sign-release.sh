#!/bin/sh
set -eu
: "${ANDROID_HOME:?Set ANDROID_HOME}"
: "${BRIDGE_KEYSTORE:?Set BRIDGE_KEYSTORE to your private signing key}"
: "${BRIDGE_PASSWORD_FILE:?Set BRIDGE_PASSWORD_FILE}"
input=${1:?Unsigned APK path required}
output=${2:?Signed APK output path required}
diagnostic=${3:-}
case "$diagnostic" in ''|--diagnostic) ;; *) echo 'Third argument must be --diagnostic or absent' >&2; exit 2 ;; esac
build_tools="$ANDROID_HOME/build-tools/34.0.0"
# AGP packages native libraries at 16 KiB boundaries. Signing preserves them;
# an older zipalign -p 4 would downgrade that alignment.
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
python3 "$root/tests/check-rust-apk.py" "$input" ${diagnostic:+--diagnostic}
"$build_tools/apksigner" sign --ks "$BRIDGE_KEYSTORE" --ks-key-alias bridge \
  --ks-pass "file:$BRIDGE_PASSWORD_FILE" --out "$output" "$input"
"$build_tools/apksigner" verify --verbose --print-certs "$output"
python3 "$root/tests/check-rust-apk.py" "$output" ${diagnostic:+--diagnostic}
