# rookery itself, for running `rookery serve` or the terminal UI without installing
# anything: the program with SSH, Python, Ansible and git beside it, and Docker's
# client for the Docker plugin. The engine is another machine's, or this
# machine's through its socket.
#
# The program is not built here. The context holds a static Linux build of
# it as `rookery`, with `requirements.txt` from the provisioning source:
#
#   rookery server image               builds it with this machine's engine
#   rookery server deploy HOST         builds it on HOST and runs the server from it
#
# CI builds the published image from the same file, with the release's own
# program in the context and ROOKERY_SOURCE=published. rookery carries this file
# and a test checks that the two are the same.
#
# Settings and host profiles live in /config with the secret key, the database
# and builds in /data. `serve` runs a PostgreSQL of its own in /data, on a socket
# only this container reaches, unless ROOKERY_DATABASE_URL names another
# database. rookery-container below does that, stops it after the server, and
# brings in the store of an older rookery in the same volumes once.
FROM debian:trixie-slim
COPY requirements.txt /tmp/requirements.txt
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl docker-buildx docker-cli docker-compose git openssh-client openssl \
      postgresql python3 python3-pip tar tini \
    && pip install --no-cache-dir --break-system-packages -r /tmp/requirements.txt \
    && apt-get purge -y python3-pip && apt-get autoremove -y \
    && rm -rf /var/lib/apt/lists/* /tmp/requirements.txt \
    && useradd --create-home --uid 1000 rookery \
    && mkdir -p /config /data && chown rookery:rookery /config /data \
    && printf '%s\n' '#!/bin/sh' 'set -eu' \
      '# serve with the database and the key this image names: made the first time, the database started' \
      '# before the server and stopped after it. Any other command, or another database, is run as it is.' \
      'own=postgres://rookery@localhost/rookery?host=/tmp/rookery-postgres' \
      'if [ "${1:-}" != serve ] || [ "${ROOKERY_DATABASE_URL:-}" != "$own" ]; then exec rookery "$@"; fi' \
      'key=${ROOKERY_SECRET_KEY_FILE:-}' \
      'if [ -n "$key" ] && [ ! -s "$key" ]; then' \
      '  mkdir -p "$(dirname "$key")"' \
      '  (umask 077; od -An -tx1 -N32 /dev/urandom | tr -d " \n" > "$key")' \
      'fi' \
      'bin=$(echo /usr/lib/postgresql/*/bin) db=/data/rookery/postgres' \
      'mkdir -p -m 0700 /tmp/rookery-postgres /data/rookery' \
      'made=' \
      'if [ ! -s "$db/PG_VERSION" ]; then "$bin/initdb" -D "$db" -U rookery -A trust -E UTF8 >/dev/null; made=1; fi' \
      '"$bin/pg_ctl" -D "$db" -l /data/rookery/postgres.log -w -s start \' \
      '  -o "-c listen_addresses= -k /tmp/rookery-postgres"' \
      'if [ -n "$made" ]; then "$bin/createdb" -h /tmp/rookery-postgres -U rookery rookery; fi' \
      '# The store of an older rookery in these volumes goes into the database, once.' \
      'from=${ROOKERY_STORE:-/data/rookery/rookery.db}; [ -n "${ROOKERY_STORE:-}" ] || [ -s "$from" ] || from=' \
      'if [ -n "$from" ] && [ ! -e /data/rookery/imported ]; then' \
      '  old=${ROOKERY_STORE_KEY:-/config/rookery/store.key}' \
      '  if { [ -s "$old" ] && rookery server import --from "$from" --key-file "$old"; } ||' \
      '     { [ ! -s "$old" ] && rookery server import --from "$from"; }; then' \
      '    date -u > /data/rookery/imported' \
      '  else "$bin/pg_ctl" -D "$db" -m fast -w -s stop; exit 1; fi' \
      'fi' \
      'rookery "$@" & server=$!' \
      'trap "kill -TERM $server 2>/dev/null" TERM INT' \
      'status=0; wait $server || status=$?' \
      'if kill -0 $server 2>/dev/null; then status=0; wait $server || status=$?; fi' \
      '"$bin/pg_ctl" -D "$db" -m fast -w -s stop' \
      'exit $status' > /usr/local/bin/rookery-container \
    && chmod 0755 /usr/local/bin/rookery-container
# The context gives it mode 0755, which COPY keeps.
COPY rookery /usr/local/bin/rookery
# The version the program has to say, and the SHA-256 it has to have. The
# build stops when either is not so, which also stops a program for another
# CPU, as it does not run here.
ARG ROOKERY_VERSION=
ARG ROOKERY_SHA256=
RUN said=$(rookery --version) && echo "$said" \
    && case "$said" in "rookery $ROOKERY_VERSION" | "rookery $ROOKERY_VERSION "*) ;; *) [ -z "$ROOKERY_VERSION" ] ;; esac \
    && { [ -z "$ROOKERY_SHA256" ] || echo "$ROOKERY_SHA256  /usr/local/bin/rookery" | sha256sum -c -; }
# `self-built` for an image built from a rookery, `published` for CI's. The
# baseline is the repository a self-built server goes back to for a release.
ARG ROOKERY_SOURCE=self-built
ARG ROOKERY_BASELINE=ghcr.io/aiviarium/rookery
LABEL org.opencontainers.image.version=$ROOKERY_VERSION \
      dev.rookery.source=$ROOKERY_SOURCE \
      dev.rookery.baseline=$ROOKERY_BASELINE \
      dev.rookery.binary-sha256=$ROOKERY_SHA256
USER rookery
ENV XDG_CONFIG_HOME=/config XDG_DATA_HOME=/data TERM=xterm-256color \
    ROOKERY_DATABASE_URL=postgres://rookery@localhost/rookery?host=/tmp/rookery-postgres \
    ROOKERY_SECRET_KEY_FILE=/config/rookery/secret.key \
    ROOKERY_LISTEN=0.0.0.0:7777 ROOKERY_ALLOW_PUBLIC=1
VOLUME ["/config", "/data"]
EXPOSE 7777
ENTRYPOINT ["tini", "--", "rookery-container"]
CMD ["serve"]
