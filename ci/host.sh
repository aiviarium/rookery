#!/usr/bin/env bash
# The test host's init. It runs sshd, takes the operator's key from /seed (or
# ROOKERY_CI_SEED, for a second test host) as a cloud provider would hand it over, and does for rookeryd what systemd does
# on a real host: start it once the role has installed it, restart it when its
# program changes and have it read its files again when they change. It runs
# tailscaled too, without a tun device, which the container has not.
set -uo pipefail
logs=/var/log/rookery-ci
seed=${ROOKERY_CI_SEED:-/seed}
ssh-keygen -A >/dev/null
/usr/sbin/sshd -D -e 2>>"$logs/sshd.log" &
sshd=$!
tailscaled=

rookeryd=
program=
files=
stop() {
  [[ -n $rookeryd ]] && kill "$rookeryd" 2>/dev/null
  [[ -n $tailscaled ]] && kill "$tailscaled" 2>/dev/null
  kill "$sshd" 2>/dev/null
  exit 0
}
trap stop TERM INT

while kill -0 "$sshd" 2>/dev/null; do
  # Started again when it ends, as systemd would: a test leaves the tailnet by
  # removing /var/lib/tailscale and stopping it.
  if [[ -z $tailscaled ]] || ! kill -0 "$tailscaled" 2>/dev/null; then
    mkdir -p /var/lib/tailscale /run/tailscale
    /usr/sbin/tailscaled --tun=userspace-networking --state=/var/lib/tailscale/tailscaled.state \
      --socket=/run/tailscale/tailscaled.sock >>"$logs/tailscaled.log" 2>&1 &
    tailscaled=$!
  fi
  for key in "$seed"/*.pub; do
    [[ -f $key ]] || continue
    user=$(basename "$key" .pub)
    if ! cmp -s "$key" "/etc/ssh/provided-keys/$user"; then
      install -m 0644 "$key" "/etc/ssh/provided-keys/$user"
    fi
  done
  if [[ -x /usr/local/bin/rookeryd && -f /etc/rookery/rookeryd.json ]]; then
    if [[ -n $rookeryd ]] && ! kill -0 "$rookeryd" 2>/dev/null; then
      echo "rookery-ci: rookeryd ended" >>"$logs/rookeryd.log"
      rookeryd=
    fi
    now=$(stat -c '%Y %s' /usr/local/bin/rookeryd)
    read_now=$(cat /etc/rookery/*.json | md5sum)
    if [[ -z $rookeryd || $now != "$program" ]]; then
      if [[ -n $rookeryd ]]; then
        kill "$rookeryd" 2>/dev/null
        wait "$rookeryd" 2>/dev/null
      fi
      echo "rookery-ci: starting rookeryd" >>"$logs/rookeryd.log"
      /usr/local/bin/rookeryd >>"$logs/rookeryd.log" 2>&1 &
      rookeryd=$!
      program=$now
      files=$read_now
    elif [[ $read_now != "$files" ]]; then
      echo "rookery-ci: reloading rookeryd" >>"$logs/rookeryd.log"
      kill -HUP "$rookeryd"
      files=$read_now
    fi
  fi
  sleep 0.5
done
echo "rookery-ci: sshd ended" >&2
exit 1
