#!/bin/sh
# Installs rookery for this user on Linux or macOS: the released program for
# this machine, checked against its SHA-256, into ~/.local/bin (or
# ROOKERY_INSTALL_DIR). On macOS rookery is a client of a rookery serve elsewhere;
# on Windows use install.ps1.
#
#   curl -fsSL https://raw.githubusercontent.com/aiviarium/rookery/main/install.sh | sh
#
# ROOKERY_VERSION picks a release tag such as v0.2.0; the default is the latest.
set -eu

repo=aiviarium/rookery
dir=${ROOKERY_INSTALL_DIR:-$HOME/.local/bin}
version=${ROOKERY_VERSION:-latest}

fail() { printf 'install: %s\n' "$1" >&2; exit 1; }

case "$(uname -s)" in
  Linux) system=linux ;;
  Darwin) system=macos ;;
  *) fail "no released program for $(uname -s): on Windows use install.ps1" ;;
esac
case "$(uname -m)" in
  x86_64|amd64) arch=x86_64 ;;
  arm64|aarch64) arch=aarch64 ;;
  *) fail "no released program for $(uname -m): build it with cargo install --locked --git https://github.com/$repo rookery" ;;
esac
asset=rookery-$arch-$system.tar.gz
for tool in curl tar; do
  command -v "$tool" >/dev/null || fail "$tool is needed"
done
# macOS has shasum rather than sha256sum.
if command -v sha256sum >/dev/null; then
  check() { sha256sum -c "$1"; }
elif command -v shasum >/dev/null; then
  check() { shasum -a 256 -c "$1"; }
else
  fail "sha256sum or shasum is needed"
fi

if [ "$version" = latest ]; then
  url=https://github.com/$repo/releases/latest/download
else
  url=https://github.com/$repo/releases/download/$version
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl -fsSL -o "$work/$asset" "$url/$asset" || fail "could not download $url/$asset"
curl -fsSL -o "$work/$asset.sha256" "$url/$asset.sha256" || fail "could not download the checksum"
(cd "$work" && check "$asset.sha256" >/dev/null) || fail "the download does not match its checksum"
tar -xzf "$work/$asset" -C "$work" rookery
mkdir -p "$dir"
install -m 755 "$work/rookery" "$dir/rookery"

printf 'Installed %s to %s\n' "$("$dir/rookery" --version 2>/dev/null || echo rookery)" "$dir/rookery"
case ":$PATH:" in
  *":$dir:"*) ;;
  *) printf 'Add %s to your PATH to run it as rookery.\n' "$dir" ;;
esac
# The hosts are set up from Linux; elsewhere rookery uses a server.
if [ "$system" != linux ]; then
  printf 'Next: rookery login URL, with the setup key of your rookery server.\n'
  exit 0
fi
printf 'Next: rookery host create NAME. rookery login URL uses a rookery server instead.\n'
missing=
for tool in ssh python3 ansible-playbook; do
  command -v "$tool" >/dev/null || missing="$missing $tool"
done
[ -z "$missing" ] || printf 'rookery also uses:%s. Install them before setting up a host.\n' "$missing"
