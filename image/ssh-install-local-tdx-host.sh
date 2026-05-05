#!/bin/bash
# Run install-local-tdx-host.sh on the configured local TDX smoke host.
#
# Usage:
#   EE_LOCAL_HOST=57.130.10.246 EE_LOCAL_USER=ubuntu \
#     image/ssh-install-local-tdx-host.sh
#
#   EE_LOCAL_HOST=57.130.10.246 EE_LOCAL_USER=ubuntu \
#     image/ssh-install-local-tdx-host.sh --install-tdx-stack
#
# Required env:
#   EE_LOCAL_HOST
#   EE_LOCAL_USER
#
# Optional env:
#   EE_LOCAL_SSH_KEY_PATH    private key path
#   TDX_CANONICAL_REF        canonical/tdx ref passed to remote installer
set -euo pipefail

INSTALL_ARGS=()
for arg in "$@"; do
    case "$arg" in
        --install-tdx-stack)
            INSTALL_ARGS+=("$arg")
            ;;
        -h|--help)
            sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "unknown argument: $arg" >&2
            exit 2
            ;;
    esac
done

: "${EE_LOCAL_HOST:?}"
: "${EE_LOCAL_USER:?}"
TDX_CANONICAL_REF="${TDX_CANONICAL_REF:-3.3}"

REMOTE_ARGS=""
if [ "${#INSTALL_ARGS[@]}" -gt 0 ]; then
    for arg in "${INSTALL_ARGS[@]}"; do
        REMOTE_ARGS+=" '$arg'"
    done
fi

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
INSTALLER="${SCRIPT_DIR}/install-local-tdx-host.sh"
[ -f "$INSTALLER" ] || { echo "missing installer: $INSTALLER" >&2; exit 2; }

SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR)
if [ -n "${EE_LOCAL_SSH_KEY_PATH:-}" ]; then
    SSH_OPTS+=(-i "$EE_LOCAL_SSH_KEY_PATH")
fi

REMOTE="${EE_LOCAL_USER}@${EE_LOCAL_HOST}"
REMOTE_INSTALLER="/tmp/easyenclave-install-local-tdx-host.sh"

echo "ssh-install-local-tdx-host: copy installer -> ${REMOTE}:${REMOTE_INSTALLER}"
scp "${SSH_OPTS[@]}" "$INSTALLER" "${REMOTE}:${REMOTE_INSTALLER}"

echo "ssh-install-local-tdx-host: run installer on ${REMOTE}"
ssh "${SSH_OPTS[@]}" "$REMOTE" \
    "chmod +x '${REMOTE_INSTALLER}' && sudo env TDX_CANONICAL_REF='${TDX_CANONICAL_REF}' bash '${REMOTE_INSTALLER}'${REMOTE_ARGS}"
