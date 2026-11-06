#!/bin/sh
# One-time Linux companion setup. There are no per-device/mode controls for the user.
set -eu
[ "$(id -u)" -eq 0 ] || { echo 'Run as root for the one-time USB/IP service installation.' >&2; exit 1; }
binary=${1:?Usage: install-agent.sh PATH_TO_AGENT TABLET_IPV4}
tablet=${2:?Tablet IPv4 required}
validated=$(python3 - "$tablet" <<'PY'
import ipaddress,sys
s=sys.argv[1];p=ipaddress.IPv4Address(s)
assert str(p)==s and not(p.is_unspecified or p.is_multicast or p.is_loopback)
assert p.is_private or p in ipaddress.IPv4Network('100.64.0.0/10')
print(str(p)+':3240')
PY
)
[ -x /usr/sbin/usbip ] || { echo 'Install usbip first.' >&2; exit 1; }
[ -f "$binary" ] || exit 1
[ -x /usr/bin/timeout ] || { echo "Install coreutils first." >&2; exit 1; }
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
install -d -m 0755 /etc/usb-most
if [ -f /etc/usb-most/tablet.conf ]; then
  cp -n /etc/usb-most/tablet.conf /etc/usb-most/tablet.conf.before-usb-most
fi
install -m 0755 "$binary" /usr/local/bin/usb-most-agent
printf 'USB_MOST_TABLET=%s\n' "$validated" > /etc/usb-most/tablet.conf
chmod 0600 /etc/usb-most/tablet.conf
install -m 0644 "$root/server/usb-most-agent.service" /etc/systemd/system/usb-most-agent.service
systemctl daemon-reload
# The old Rutoken-only reconnect timer competes for the same exported device.
if systemctl cat tablet-usbip.timer >/dev/null 2>&1; then
  systemctl disable --now tablet-usbip.timer
fi
systemctl enable --now usb-most-agent.service
systemctl --no-pager status usb-most-agent.service
