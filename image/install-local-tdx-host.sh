#!/bin/bash
# Prepare a remote/local host for easyenclave local-tdx-qcow2 smoke.
#
# This script is intentionally explicit about the risky step:
# - base QEMU/ISO tooling can be installed idempotently
# - TDX host stack installation changes kernel/QEMU/firmware packages and
#   requires a reboot, so it only runs with --install-tdx-stack
#
# Usage:
#   sudo image/install-local-tdx-host.sh
#   sudo image/install-local-tdx-host.sh --install-tdx-stack
#
# Environment:
#   TDX_CANONICAL_REF  canonical/tdx git ref to use (default: 3.3)
set -euo pipefail

INSTALL_TDX_STACK=0
for arg in "$@"; do
    case "$arg" in
        --install-tdx-stack) INSTALL_TDX_STACK=1 ;;
        -h|--help)
            sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "unknown argument: $arg" >&2
            exit 2
            ;;
    esac
done

if [ "$(id -u)" -ne 0 ]; then
    echo "run as root, e.g. sudo $0" >&2
    exit 2
fi

. /etc/os-release
if [ "${ID:-}" != "ubuntu" ]; then
    echo "unsupported OS: ${PRETTY_NAME:-unknown}; expected Ubuntu" >&2
    exit 2
fi

case "${VERSION_ID:-}" in
    24.04|25.04|25.10) ;;
    *)
        echo "unsupported Ubuntu version: ${VERSION_ID:-unknown}" >&2
        echo "Canonical TDX host support is release-sensitive; use Ubuntu 24.04/25.04 tech preview or 25.10+." >&2
        exit 2
        ;;
esac

echo "local-tdx-host: installing base tooling"
apt-get update
DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    ca-certificates curl git jq \
    qemu-system-x86 qemu-utils genisoimage ovmf

tdx_param() {
    cat /sys/module/kvm_intel/parameters/tdx 2>/dev/null || true
}

qemu_has_tdx() {
    qemu-system-x86_64 -object help 2>/dev/null | grep -Eq '^[[:space:]]*tdx-guest$'
}

tdvf_path() {
    local dir
    for dir in /usr/local/share /usr/share /opt; do
        [ -d "$dir" ] || continue
        find "$dir" -xdev \
            \( -iname 'OVMF.inteltdx*.fd' -o -iname 'OVMF.tdx.fd' -o -iname 'TDVF*.fd' \) \
            -type f 2>/dev/null | head -1
    done | head -1
}

echo "local-tdx-host: current status"
echo "  kernel: $(uname -r)"
echo "  kvm:    $(test -e /dev/kvm && echo yes || echo no)"
echo "  tdx:    $(tdx_param || true)"
echo "  qemu:   $(qemu_has_tdx && echo tdx-guest || echo no-tdx-guest)"
echo "  tdvf:   $(tdvf_path || true)"

if [ "$(tdx_param)" = "Y" ] && qemu_has_tdx && [ -n "$(tdvf_path)" ]; then
    echo "local-tdx-host: TDX host stack already present"
    exit 0
fi

if [ "$INSTALL_TDX_STACK" -ne 1 ]; then
    cat >&2 <<'EOF'
local-tdx-host: TDX host stack is not complete.

Re-run with --install-tdx-stack to install Canonical's TDX host preview
stack. That may replace kernel/QEMU/firmware packages and requires a reboot.
EOF
    exit 3
fi

TDX_CANONICAL_REF="${TDX_CANONICAL_REF:-3.3}"
WORKDIR="/opt/easyenclave-tdx-host"

echo "local-tdx-host: installing Canonical TDX host stack ref=${TDX_CANONICAL_REF}"
rm -rf "$WORKDIR"
git clone --depth 1 --branch "$TDX_CANONICAL_REF" https://github.com/canonical/tdx.git "$WORKDIR"

cd "$WORKDIR"
if [ -f setup-tdx-config ]; then
    # Host smoke does not need host-side remote attestation packages. The
    # guest itself still uses configfs-tsm for quote generation.
    sed -i 's/^TDX_SETUP_ATTESTATION=.*/TDX_SETUP_ATTESTATION=0/' setup-tdx-config || true
fi

./setup-tdx-host.sh

cat >&2 <<'EOF'
local-tdx-host: TDX host stack installer finished.

Reboot the host, then verify:
  cat /sys/module/kvm_intel/parameters/tdx
  qemu-system-x86_64 -object help | grep -E '^[[:space:]]*tdx-guest$'
  find /usr/local/share /usr/share /opt -iname '*TDVF*.fd' -o -iname '*inteltdx*.fd' -o -iname 'OVMF.tdx.fd'
EOF
