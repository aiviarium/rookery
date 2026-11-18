<h1><a href="https://github.com/aiviarium/rookery"><picture><source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/aiviarium/.github/main/brand/lockups/lockup-rookery.svg"><img src="https://raw.githubusercontent.com/aiviarium/.github/main/brand/lockups/lockup-rookery-light.svg" alt="Rookery" height="56"></picture></a><br><a href="https://github.com/aiviarium/rookery/actions/workflows/ci.yml"><img src="https://aiviarium-badges.helight.workers.dev/rookery/github/actions/workflow/status/aiviarium/rookery/ci.yml?branch=main&label=CI" alt="CI"></a> <a href="https://github.com/aiviarium/rookery/releases"><img src="https://aiviarium-badges.helight.workers.dev/rookery/github/v/release/aiviarium/rookery?include_prereleases" alt="Release"></a> <a href="#license"><img src="https://aiviarium-badges.helight.workers.dev/rookery/github/license/aiviarium/rookery" alt="License"></a></h1>

Provisioning and management for development machines.

Rookery sets up Ubuntu 26.04 hosts on a spare laptop, a server, a cloud machine
or a container, and manages them from a terminal UI and a command line.
Provisioner plugins create the machines. A machine that already runs can be a
host too: `rookery host add forge 192.0.2.4 --user ops`.

Each host has a profile that Rookery installs and can apply again, and a page
with its status, sign-ins, updates and secrets. A host runs XFCE on a virtual
display that opens in a browser, and Cockpit for administration. The software
on a host, coding tools among it, comes from plugins.

<img src="docs/public/screens/overview.png" alt="Rookery's terminal UI showing the overview of a host with its actions">

Rookery is at an early stage. **The installer erases the disk it selects**, so a
profile is best tried in the container or a VM before it boots on real hardware.

## Install

On Linux or macOS:

```bash
curl -fsSL https://raw.githubusercontent.com/aiviarium/rookery/main/install.sh | sh
```

The script installs the latest release to `~/.local/bin` after checking its
SHA-256. On other architectures, build it:

```bash
rustup target add x86_64-unknown-linux-musl
cargo install --locked --git https://github.com/aiviarium/rookery rookery
```

Setting up hosts also needs `ssh`, `python3` and `ansible-playbook` on your
machine. Rookery is also an image, `ghcr.io/aiviarium/rookery`, which runs its
local API server by default.

On macOS and Windows, Rookery works with the hosts of a Rookery server elsewhere,
with the same screens. `install.sh` covers macOS. On Windows:

```powershell
irm https://raw.githubusercontent.com/aiviarium/rookery/main/install.ps1 | iex
```

## Quick start

```bash
rookery studio          # a new host: choose how to install, features and settings
rookery                 # the home screen with all hosts
rookery run studio status
```

A new host opens on its Install tab. It offers five install methods. A plugin
creates a machine, or the host boots a USB installer or a cloud-init seed, or
Rookery connects over SSH to an existing Ubuntu server, or runs a container image. Features and Settings follow, and Run installs
it. Once the host runs, `rookery studio` opens its overview, with the sign-ins
and the address of its desktop.

## Documentation

The documentation is at [rookery.helight.dev](https://rookery.helight.dev/). The site is built from [`docs/`](docs) (`cd docs && npm install && npm run dev`).

- [Getting started](docs/content/docs/%28guide%29/start/install.mdx): installing Rookery and a first host.
- [Deploying](docs/content/docs/%28guide%29/deploy/index.mdx): the install methods, and testing in QEMU.
- [Using Rookery](docs/content/docs/%28guide%29/use/index.mdx): the terminal UI and commands, sign-ins, secrets and updates.
- [Extending](docs/content/docs/%28guide%29/extend/index.mdx): plugins, services and `rookery serve`.
- [Reference](docs/content/docs/%28guide%29/reference/index.mdx): tools, `box`, profile fields, building and releasing.
- The API reference of rookeryd and `rookery serve`, generated from [`docs/openapi/`](docs/openapi).

## Contributing and security

[CONTRIBUTING.md](.github/CONTRIBUTING.md) says what Rookery is meant to be and how
changes get in. A pull request starts with an issue. [SECURITY.md](.github/SECURITY.md)
says how to report a vulnerability and where Rookery's API is meant to be reachable from.

## License

Licensed under either of the [Apache License, Version 2.0](LICENSE-APACHE) or
the [MIT license](LICENSE-MIT), at your option. Contributions are licensed the same way.

The vendored `tui-input` crate under `crates/vendor` has its own MIT license. The
tools that plugins install, such as coding tools and Unity, are separate
products under their vendors' terms. Rookery doesn't include them,
and a plugin downloads them from their vendors.
