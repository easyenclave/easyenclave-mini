#!/usr/bin/env bash
set -euo pipefail

TARGET="${TARGET:?TARGET is required}"
SHA12="${SHA12:?SHA12 is required}"
GITHUB_SHA="${GITHUB_SHA:?GITHUB_SHA is required}"

cd "image/output/${TARGET}"

UKI_SHA256=$(sha256sum easyenclave.efi | cut -d' ' -f1)
mv easyenclave.efi "easyenclave-mini-${SHA12}-${TARGET}.efi"

{
    echo "commit: ${GITHUB_SHA}"
    echo "target: ${TARGET}"
    echo "UKI sha256: ${UKI_SHA256}"
} > MEASUREMENTS.txt

if [ -f easyenclave.root.raw ]; then
    RAW_SHA=$(sha256sum easyenclave.root.raw | cut -d' ' -f1)
    mv easyenclave.root.raw "easyenclave-mini-${SHA12}-${TARGET}.raw"
    echo "Disk sha256: ${RAW_SHA}" >> MEASUREMENTS.txt
fi

if [ -f easyenclave.qcow2 ]; then
    QCOW_SHA=$(sha256sum easyenclave.qcow2 | cut -d' ' -f1)
    mv easyenclave.qcow2 "easyenclave-mini-${SHA12}-${TARGET}.qcow2"
    echo "Qcow2 sha256: ${QCOW_SHA}" >> MEASUREMENTS.txt
fi

if [ -f easyenclave-gcp.tar.gz ]; then
    mv easyenclave-gcp.tar.gz "easyenclave-mini-${SHA12}-gcp.tar.gz"
fi

if [ -f easyenclave.vhd ]; then
    VHD_SHA=$(sha256sum easyenclave.vhd | cut -d' ' -f1)
    mv easyenclave.vhd "easyenclave-mini-${SHA12}-${TARGET}.vhd"
    echo "VHD sha256: ${VHD_SHA}" >> MEASUREMENTS.txt
fi

ls -la
