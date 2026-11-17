# The boot tests: an Ubuntu cloud image with rookery's cloud seed, and rookery's
# unattended installer on a blank virtual disk, each booted in QEMU, set up on
# first boot and then used with rookery over SSH. Sourced by ci/run.sh, whose
# compose and dev functions it uses.
#
#   ci/run.sh boot [cloud] [iso] [--keep] [--tcg] [-- TEST…]
#
# Without cloud or iso, both run. --keep leaves the guest running after the
# tests until Ctrl-C and says how to reach it. --tcg runs the guest without
# KVM, which takes hours. Downloads, the package cache, logs and timings are in
# build/boot.
#
# ROOKERY_CI_BOOT_IMAGE       the image (default: rookery-ci-boot)
# ROOKERY_CI_BOOT_ISO         an Ubuntu Server ISO already on this machine, used
#                           read-only; otherwise it is downloaded to build/boot
# ROOKERY_CI_BOOT_ISO_SHA256  the SHA-256 it must have (default: Ubuntu's SHA256SUMS)
# ROOKERY_CI_BOOT_MEMORY      the guest's memory in MiB (default: 4096)
# ROOKERY_CI_BOOT_EXTRAS      1 installs the programs a host can do without too
# ROOKERY_CI_BOOT_CPUS        its vCPUs (default: 4, at most what is there)
# ROOKERY_CI_BOOT_SSH_PORT    the loopback port of its SSH with --keep (default: 22722)
# ROOKERY_CI_BOOT_CONTROL_URL, ROOKERY_CI_BOOT_CONTROL_KEY
#                           a tailnet control server, a Headscale address or
#                           tailscale for Tailscale's own, and its API key, with
#                           which rookery makes the key the guest joins with on
#                           its first boot
# ROOKERY_CI_BOOT_CONTROL_USER, ROOKERY_CI_BOOT_CONTROL_TAGS
#                           rookery's tailnet_user and tailnet_tags for it

boot_compose() { "$engine" compose -f ci/boot.compose.yml "$@"; }

boot() {
  local kinds=() options=() tests=() keep=0 tcg=${ROOKERY_CI_BOOT_TCG:-0}
  while (($#)); do
    case $1 in
      cloud | iso) kinds+=("$1") ;;
      --keep) keep=1; options+=(--keep) ;;
      --tcg) tcg=1 ;;
      --) shift; tests=("$@"); break ;;
      *) echo "ci/run.sh boot takes cloud, iso, --keep, --tcg and -- TEST…, not $1" >&2; return 2 ;;
    esac
    shift
  done
  ((${#kinds[@]})) || kinds=(cloud iso)

  if [[ $tcg == 1 ]]; then
    echo "Running the guest without KVM: expect hours rather than minutes." >&2
    export ROOKERY_CI_KVM=/dev/null ROOKERY_CI_BOOT_TCG=1
  elif [[ ! -c /dev/kvm ]]; then
    echo "The boot tests need KVM and there is no /dev/kvm here. Turn on virtualisation," >&2
    echo "load kvm_intel or kvm_amd, or pass --tcg to emulate the guest very slowly." >&2
    return 1
  elif [[ ! -r /dev/kvm || ! -w /dev/kvm ]] && [[ $engine == podman && $(id -u) != 0 ]]; then
    echo "/dev/kvm is there but $(id -un) cannot open it: join the kvm group, or pass --tcg." >&2
    return 1
  fi
  if [[ -n ${ROOKERY_CI_BOOT_ISO:-} ]]; then
    if [[ ! -f $ROOKERY_CI_BOOT_ISO ]]; then
      echo "ROOKERY_CI_BOOT_ISO is not a file: $ROOKERY_CI_BOOT_ISO" >&2
      return 1
    fi
    ROOKERY_CI_BOOT_ISO=$(cd "$(dirname "$ROOKERY_CI_BOOT_ISO")" && pwd)/$(basename "$ROOKERY_CI_BOOT_ISO")
    export ROOKERY_CI_BOOT_ISO ROOKERY_BOOT_ISO_NAME=${ROOKERY_CI_BOOT_ISO##*/}
  fi
  mkdir -p build/boot
  if [[ ${ROOKERY_CI_NO_BUILD:-} != 1 ]]; then
    build dev
    boot_compose build --quiet
  fi
  # The guest gets rookery's host programs from the role's files, which this build puts there.
  dev cargo build --locked -p rookery

  local kind status=0 run
  for kind in "${kinds[@]}"; do
    run=(run --rm "${tty[@]}")
    ((keep)) && run+=(--service-ports)
    boot_compose "${run[@]}" vm python3 tests/e2e/boot.py "$kind" "${options[@]}" "${tests[@]}" || status=1
    # What root wrote there in the container belongs to the user again.
    if [[ $ROOKERY_CI_USER != 0:0 ]]; then
      boot_compose run --rm -T --no-deps --entrypoint chown vm -R "$ROOKERY_CI_USER" /work >/dev/null 2>&1 || true
    fi
  done
  boot_compose down --remove-orphans --timeout 5 >/dev/null 2>&1 || true
  return "$status"
}
