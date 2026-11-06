#!/bin/sh
# Fixed product probe. No payload from stdin is executed as code.
IFS= read -r nonce || exit 70
[ "${#nonce}" -eq 32 ] || exit 70
case "$nonce" in *[!0-9a-f]*) exit 70 ;; esac
system=$(uname -s 2>/dev/null) || exit 71
case "$system" in Linux) ;; MINGW*|MSYS*|CYGWIN*) exit 71 ;; *) exit 72 ;; esac
machine=$(uname -m 2>/dev/null) || exit 72
case "$machine" in x86_64|amd64) arch=x86_64 ;; aarch64|arm64) arch=aarch64 ;; *) exit 72 ;; esac
elevated=false
[ "$(id -u)" -eq 0 ] && elevated=true
stack=false
if [ -w /sys/devices/platform/vhci_hcd.0/attach ] || [ -d /sys/module/vhci_hcd ]; then stack=true; fi
version=null
hash=null
binary=/usr/local/libexec/usb-most/usb-most-desktop
# Never execute a pre-created or writable lookalike during advisory preflight.
trusted=true
for path in /usr /usr/local /usr/local/libexec /usr/local/libexec/usb-most "$binary"; do
  if [ -L "$path" ] || [ "$(stat -c %u -- "$path" 2>/dev/null)" != 0 ]; then trusted=false; break; fi
  mode=$(stat -c %a -- "$path" 2>/dev/null) || { trusted=false; break; }
  if [ "$((0$mode & 022))" -ne 0 ]; then trusted=false; break; fi
done
if [ "$trusted" = true ] && [ -f "$binary" ] && [ -x "$binary" ] && command -v timeout >/dev/null 2>&1; then
  candidate=$(timeout 5s "$binary" --version 2>/dev/null)
  if [ "$?" -eq 0 ] && [ -n "$candidate" ] && [ "${#candidate}" -le 32 ]; then
    case "$candidate" in *[!0-9A-Za-z.-]*) ;; *) version="\"$candidate\"" ;; esac
  fi
fi
if [ "$trusted" = true ] && [ -f "$binary" ] && command -v sha256sum >/dev/null 2>&1 && command -v timeout >/dev/null 2>&1; then
  candidate=$(timeout 5s sha256sum "$binary" 2>/dev/null) || exit 75
  candidate=${candidate%% *}
  if [ "${#candidate}" -eq 64 ]; then
    case "$candidate" in *[!0-9a-f]*) ;; *) hash="\"$candidate\"" ;; esac
  fi
fi
running=false
if command -v systemctl >/dev/null 2>&1 && command -v timeout >/dev/null 2>&1; then
  timeout 5s systemctl is-active --quiet usb-most-desktop.service 2>/dev/null
  result=$?
  [ "$result" -eq 124 ] && exit 75
  [ "$result" -eq 0 ] && running=true
fi
reboot=false
[ -f /var/run/reboot-required ] && reboot=true
systemd=false
[ -d /run/systemd/system ] && systemd=true
module=unknown
if [ "$stack" = true ]; then
  module=available
elif command -v timeout >/dev/null 2>&1; then
  for tool in /usr/sbin/modinfo /sbin/modinfo; do
    [ -x "$tool" ] || continue
    timeout 5s "$tool" -n vhci_hcd >/dev/null 2>&1
    result=$?
    case "$result" in 0) module=available ;; 1) module=unavailable ;; esac
    break
  done
fi
printf '{"schema":1,"nonce":"%s","os":"linux","arch":"%s","elevated":%s,"usb_stack":%s,"agent_version":%s,"agent_sha256":%s,"service_running":%s,"reboot_pending":%s,"linux_prerequisites":{"systemd":%s,"module":"%s"}}\n' "$nonce" "$arch" "$elevated" "$stack" "$version" "$hash" "$running" "$reboot" "$systemd" "$module"
