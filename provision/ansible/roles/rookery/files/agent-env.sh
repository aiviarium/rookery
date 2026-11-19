#!/usr/bin/env bash
set -euo pipefail
source /etc/rookery/environment
if [[ -n "${XDG_RUNTIME_DIR:-}" && ! -w "$XDG_RUNTIME_DIR" ]]; then
    unset XDG_RUNTIME_DIR
fi
if [[ -n "${XDG_RUNTIME_DIR:-}" ]]; then
    export DBUS_SESSION_BUS_ADDRESS=${DBUS_SESSION_BUS_ADDRESS:-unix:path=$XDG_RUNTIME_DIR/bus}
fi
if [[ $# == 0 ]]; then exec /bin/bash; fi
exec "$@"
