#!/bin/sh
# The entrypoint of rookery's image. `serve` with the database this image names runs a PostgreSQL of
# its own in /data/rookery/postgres, reached on a socket inside the container alone: it is made the
# first time, started before the server and stopped after it. The server connects as the role
# rookery, which owns its database and is no superuser. Any other command, or a server given another
# database, runs as it is.
#
# Give the container time to stop both: `docker stop -t 30`, or `stop_grace_period: 30s` in Compose.
set -eu

own=postgres://rookery@localhost/rookery?host=/tmp/rookery-postgres
if [ "${1:-}" != serve ] || [ "${ROOKERY_DATABASE_URL:-}" != "$own" ]; then
  exec rookery "$@"
fi

# The major version of PostgreSQL in this image; a data folder of another one is not opened.
major=17
bin=/usr/lib/postgresql/$major/bin
db=/data/rookery/postgres
socket=/tmp/rookery-postgres

key=${ROOKERY_SECRET_KEY_FILE:-}
if [ -n "$key" ] && [ ! -s "$key" ]; then
  mkdir -p "$(dirname "$key")"
  (umask 077; od -An -tx1 -N32 /dev/urandom | tr -d ' \n' > "$key")
fi

mkdir -p -m 0700 "$socket" /data/rookery
if [ -s "$db/PG_VERSION" ]; then
  found=$(cat "$db/PG_VERSION")
  if [ "$found" != "$major" ]; then
    echo "rookery: $db is a database of PostgreSQL $found, and this image has PostgreSQL $major." >&2
    echo "rookery: dump it with the image that made it (pg_dumpall), and restore the dump into a new folder." >&2
    exit 1
  fi
elif [ -d "$db" ] && [ -n "$(ls -A "$db")" ]; then
  echo "rookery: $db holds files that are no PostgreSQL data folder, and they are left as they are." >&2
  echo "rookery: move them away to let this image make its database there." >&2
  exit 1
else
  # Trust on the socket alone: it is in a folder of mode 0700 inside this container, so only this
  # container's own user reaches it, and the server's role is no superuser.
  "$bin/initdb" -D "$db" -U postgres -A trust -E UTF8 >/dev/null
fi
"$bin/pg_ctl" -D "$db" -l /data/rookery/postgres.log -w -s start \
  -o "-c listen_addresses= -k $socket"
stop_database() { "$bin/pg_ctl" -D "$db" -m fast -w -s stop; }

# The role and its database, made whenever they are missing, as after a stop between the two.
sql() { "$bin/psql" -h "$socket" -U postgres -d postgres -v ON_ERROR_STOP=1 -tAq "$@"; }
if [ -z "$(sql -c "SELECT 1 FROM pg_roles WHERE rolname = 'rookery'")" ]; then
  sql -c "CREATE ROLE rookery LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE"
fi
if [ -z "$(sql -c "SELECT 1 FROM pg_database WHERE datname = 'rookery'")" ]; then
  "$bin/createdb" -h "$socket" -U postgres -O rookery rookery
fi

# The store of an older rookery in these volumes goes into the database, once: the database says
# whether it had its import. A URL, which may hold a password, goes in a private file.
store=/data/rookery/rookery.db
if [ -n "${ROOKERY_STORE:-}" ] || [ -s "$store" ]; then
  given=$(mktemp)
  chmod 600 "$given"
  printf '%s\n' "${ROOKERY_STORE:-$store}" > "$given"
  old=${ROOKERY_STORE_KEY:-/config/rookery/store.key}
  if [ -s "$old" ]; then
    set -- --key-file "$old"
  else
    set --
  fi
  if ! rookery server import --if-not-imported --owner admin --from-file "$given" "$@"; then
    rm -f "$given"
    stop_database
    exit 1
  fi
  rm -f "$given"
  set -- serve
fi

rookery "$@" &
server=$!
trap 'kill -TERM "$server" 2>/dev/null' TERM INT
status=0
wait "$server" || status=$?
if kill -0 "$server" 2>/dev/null; then
  status=0
  wait "$server" || status=$?
fi
stop_database
exit "$status"
