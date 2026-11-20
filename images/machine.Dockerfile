# A machine for the Docker provisioner.
#
# Ubuntu 24.04 with systemd as PID 1, OpenSSH, Python and cloud-init's NoCloud
# datasource. The provisioner copies the seed Rookery rendered into
# /var/lib/cloud/seed/nocloud before the machine starts, so the machine comes up
# with the operator account and the tailnet, and the apply does the rest.
#
# systemd needs to be PID 1 and to own its cgroups. Run this with the sysbox
# runtime when the engine has it, or otherwise unprivileged with its own cgroup
# namespace and only the capabilities systemd needs. Never privileged: a
# privileged systemd reads the host's udev and its lid switch, and has suspended
# the machine that ran it. What it would react to is masked below as well.
FROM ubuntu:24.04

ENV container=docker
ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
        ca-certificates \
        cloud-init \
        curl \
        dbus \
        iproute2 \
        iputils-ping \
        less \
        openssh-server \
        python3 \
        rsync \
        sudo \
        systemd \
        systemd-sysv \
 && rm -rf /var/lib/apt/lists/*

# The host's power state and its hardware are none of a machine's business, and a
# container must not react to them: logind, the udev units and every way of
# sleeping are masked. The provisioner's containers mask nothing themselves, so
# the image has to.
RUN systemctl mask \
        systemd-logind.service \
        systemd-udevd.service \
        systemd-udevd-control.socket \
        systemd-udevd-kernel.socket \
        systemd-udev-trigger.service \
        sleep.target \
        suspend.target \
        hibernate.target \
        hybrid-sleep.target \
        suspend-then-hibernate.target \
 && systemctl enable ssh.service

# cloud-init reads its seed from the NoCloud volume and configures no network of
# its own: the machine's network is the container's.
RUN printf '%s\n' \
        'datasource_list: [NoCloud, None]' \
        'network: {config: disabled}' \
        > /etc/cloud/cloud.cfg.d/99-rookery.cfg \
 && rm -f /etc/cloud/cloud.cfg.d/subiquity-disable-cloudinit-networking.cfg \
 && mkdir -p /var/lib/cloud/seed/nocloud

STOPSIGNAL SIGRTMIN+3

CMD ["/sbin/init"]
