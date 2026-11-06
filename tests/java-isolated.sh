#!/bin/sh
# Host network tests own ports 3240/etc. An unprivileged user/network namespace
# prevents collisions with another test or the developer's running receiver.
# No host interface, route, service, privilege or firewall is modified.
set -eu
exec unshare --user --map-root-user --net sh -c '
  /usr/sbin/ip link set lo up
  exec java "$@"
' java-fixture "$@"
