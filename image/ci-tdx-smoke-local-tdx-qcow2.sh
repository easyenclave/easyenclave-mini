#!/bin/bash
# Local-TDX-qcow2 real-TDX integration test — hosted-side driver.
#
# GitHub-hosted runners don't have TDX. SSH into EE_LOCAL_HOST,
# scp the qcow2 artifact there, invoke the local runner which boots it
# under real OVMF.inteltdx.fd +
# kvm_intel.tdx=Y. Mirrors the pattern dd's relaunch-* actions use.
#
# Why qcow2 (not ISO): dd's production path is libvirt with qcow2 as a
# COW backing file. This test validates the same artifact shape dd
# consumes on every release, not a dev-only ISO.
#
# Required env:
#   SHA12                    commit sha12 (for artifact name)
#   GITHUB_SHA               full commit sha (for remote checkout)
#   EE_LOCAL_HOST            real TDX hostname or IP
#   EE_LOCAL_USER            SSH user on EE_LOCAL_HOST
#   EE_LOCAL_SSH_KEY_PATH    path to the private key file
# Optional env:
#   EE_LOCAL_REPO            repo checkout path on EE_LOCAL_HOST
#                            (default: /home/${EE_LOCAL_USER}/src/easyenclave)
#   EE_LOCAL_REPO_URL        remote URL to fetch before smoke
#                            (default: existing origin URL)
set -euo pipefail

: "${SHA12:?}"
: "${GITHUB_SHA:?}"
: "${EE_LOCAL_HOST:?}"
: "${EE_LOCAL_USER:?}"
: "${EE_LOCAL_SSH_KEY_PATH:?}"
EE_LOCAL_REPO="${EE_LOCAL_REPO:-/home/${EE_LOCAL_USER}/src/easyenclave}"

QCOW2="image/output/local-tdx-qcow2/easyenclave-mini-${SHA12}-local-tdx-qcow2.qcow2"
[ -f "$QCOW2" ] || { echo "missing $QCOW2" >&2; exit 2; }

SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i "$EE_LOCAL_SSH_KEY_PATH")
REMOTE_QCOW2="/tmp/easyenclave-smoke-${SHA12}.qcow2"
REMOTE="${EE_LOCAL_USER}@${EE_LOCAL_HOST}"

cleanup() {
    set +e
    echo "smoke:local-tdx-qcow2: cleanup remote qcow2"
    ssh "${SSH_OPTS[@]}" "$REMOTE" "rm -f '${REMOTE_QCOW2}'" 2>/dev/null || true
}
trap cleanup EXIT

echo "smoke:local-tdx-qcow2: scp $QCOW2 -> ${REMOTE}:${REMOTE_QCOW2}"
scp "${SSH_OPTS[@]}" "$QCOW2" "${REMOTE}:${REMOTE_QCOW2}"

echo "smoke:local-tdx-qcow2: ssh + run runner"
ssh "${SSH_OPTS[@]}" "$REMOTE" "bash -s" <<REMOTE_SCRIPT
set -euo pipefail
cd "${EE_LOCAL_REPO}"
if [ -n "${EE_LOCAL_REPO_URL:-}" ]; then
    git remote set-url origin "${EE_LOCAL_REPO_URL}"
fi
# CI workspace, not a dev checkout — force-reset to the SHA under test
# so the runner script we invoke matches the commit.
git fetch --quiet origin ${GITHUB_SHA}
git reset --quiet --hard ${GITHUB_SHA}
git clean -qfd
actual=\$(git rev-parse HEAD)
if [ "\$actual" != "${GITHUB_SHA}" ]; then
    echo "remote HEAD \$actual != expected ${GITHUB_SHA}" >&2
    exit 2
fi
exec bash image/ci-tdx-smoke-local-tdx-qcow2-runner.sh "${REMOTE_QCOW2}" "${SHA12}"
REMOTE_SCRIPT
