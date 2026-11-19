# rookery itself, for running `rookery serve` or the terminal UI without installing
# anything: the program with SSH, Python, Ansible and git beside it, and Docker's
# client for the Docker plugin. The engine is another machine's, or this
# machine's through its socket.
#
# The program is not built here. The context holds a static Linux build of
# it as `rookery`, with `requirements.txt` from the provisioning source and
# `rookery-container.sh`, the entrypoint, from container/:
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
# database. rookery-container.sh does that, stops it after the server, and
# brings in the store of an older rookery in the same volumes once. Give a
# container 30 seconds to stop: docker stop -t 30, or stop_grace_period.
FROM debian:trixie-slim
COPY requirements.txt /tmp/requirements.txt
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl docker-buildx docker-cli docker-compose git openssh-client openssl \
      postgresql-17 python3 python3-pip tar tini \
    && pip install --no-cache-dir --break-system-packages -r /tmp/requirements.txt \
    && apt-get purge -y python3-pip && apt-get autoremove -y \
    && rm -rf /var/lib/apt/lists/* /tmp/requirements.txt \
    && useradd --create-home --uid 1000 rookery \
    && mkdir -p /config /data && chown rookery:rookery /config /data
# The context gives them mode 0755, which COPY keeps.
COPY rookery rookery-container.sh /usr/local/bin/
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
ENTRYPOINT ["tini", "--", "rookery-container.sh"]
CMD ["serve"]
