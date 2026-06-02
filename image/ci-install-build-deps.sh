#!/usr/bin/env bash
set -euo pipefail

KERNEL_META="${KERNEL_META:?KERNEL_META is required}"
KERNEL_FLAVOR="${KERNEL_FLAVOR:?KERNEL_FLAVOR is required}"
GITHUB_OUTPUT="${GITHUB_OUTPUT:?GITHUB_OUTPUT is required}"

sudo apt-get -o Acquire::Retries=5 update

mapfile -t kernel_pkgs < <(
    apt-cache depends --no-recommends "$KERNEL_META" \
        | awk '
            /Depends:/ {
              dep=$2
              gsub(/[<>]/, "", dep)
              if (dep ~ /^linux-image-[0-9].*-(generic|azure|gcp)$/ ||
                  dep ~ /^linux-modules-extra-[0-9].*-generic$/) {
                print dep
              }
            }' \
        | sort -u
)

if [ "${#kernel_pkgs[@]}" -eq 0 ]; then
    echo "::error::failed to resolve kernel packages from $KERNEL_META" >&2
    apt-cache depends --no-recommends "$KERNEL_META" >&2
    exit 2
fi

printf 'kernel packages:'
printf ' %s' "${kernel_pkgs[@]}"
printf '\n'

# No busybox-static (the initrd ships the static ee-init; the rootfs uses
# easyenclave's multicall sh/httpd) and no cryptsetup-bin (dm-verity deferred).
# musl-tools provides the linker for the static ee-init build.
sudo apt-get -o Acquire::Retries=5 install -y --no-install-recommends \
    systemd-boot-efi systemd-ukify mtools musl-tools \
    e2fsprogs dosfstools qemu-utils \
    zstd "${kernel_pkgs[@]}"

KVER=$(ls -1 /boot/vmlinuz-*-"$KERNEL_FLAVOR" 2>/dev/null \
    | sed 's|/boot/vmlinuz-||' | sort -V | tail -1)
[ -n "$KVER" ] || KVER=$(uname -r)
echo "kver=$KVER" >> "$GITHUB_OUTPUT"
