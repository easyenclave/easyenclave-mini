#!/bin/bash
# Build a minimal initrd for easyenclave VMs, profile-driven.
# Just enough to: load the target's modules, mount its root, switch_root —
# all done by the static Rust `ee-init` binary that becomes /init. No busybox,
# no shell scripts in the initrd (see image/init/). ~1-2MB.
#
# Usage: mkinitrd.sh <outfile> <kernel-version> <profile-env> <ee-init-binary>
#
# The profile env file (e.g. image/targets/gcp/profile.env) supplies:
#   TARGET_INITRD_MODULES   - space-separated module names to pull in
# (TARGET_ROOT_STRATEGY / TARGET_VENDOR are now documentation-only: ee-init
#  carries all root/vendor logic and selects the vendor at runtime via the
#  baked `ee.vendor=` cmdline param.)
set -euo pipefail

OUTFILE="${1:?Usage: mkinitrd.sh <outfile> <kernel-version> <profile-env> <ee-init-binary>}"
KVER="${2:?Usage: mkinitrd.sh <outfile> <kernel-version> <profile-env> <ee-init-binary>}"
PROFILE="${3:?Usage: mkinitrd.sh <outfile> <kernel-version> <profile-env> <ee-init-binary>}"
EE_INIT_BIN="${4:?Usage: mkinitrd.sh <outfile> <kernel-version> <profile-env> <ee-init-binary>}"
MOD_SRC="/lib/modules/$KVER"

[ -f "$PROFILE" ] || { echo "FATAL: profile $PROFILE not found"; exit 1; }
[ -d "$MOD_SRC" ] || { echo "FATAL: $MOD_SRC not found"; exit 1; }
[ -f "$EE_INIT_BIN" ] || { echo "FATAL: ee-init binary not found at $EE_INIT_BIN (build: cargo build -p ee-init --profile release-min --target x86_64-unknown-linux-musl)"; exit 1; }

# shellcheck disable=SC1090
. "$PROFILE"

echo "Building initrd for kernel $KVER"
echo "  profile: $PROFILE"
echo "  init:    $EE_INIT_BIN"
echo "  modules: $TARGET_INITRD_MODULES"

WORKDIR=$(mktemp -d)
trap "rm -rf $WORKDIR" EXIT

# Only mountpoints + the module tree; the rest of userspace is the ee-init ELF.
mkdir -p "$WORKDIR"/{dev,proc,sys,tmp,mnt/root}

# Copy modules + full transitive dep tree using modprobe's resolution.
# modprobe --show-depends is the source of truth — don't hand-list deps.
# Preserve the kernel/... path structure so modules.dep entries still resolve
# in the initrd.
#
# Modules are decompressed as we go. Ubuntu kernels ship Zstd-compressed
# modules (.ko.zst) and ee-init loads them with the legacy init_module(2)
# syscall (whole-image), which cannot handle Zstd — the kernel would see
# raw Zstd bytes and print "Invalid ELF header magic". finit_module(...,
# MODULE_INIT_COMPRESSED_FILE) would let the kernel decompress, but we keep
# ee-init minimal. So decompress once at build time and let ee-init load
# plain .ko files in modules.dep dependency order.
#
# Some modules may be compiled into the kernel (CONFIG_*=y) instead of
# shipped as .ko files. In that case `modprobe --show-depends` returns
# nothing and we must skip silently, not abort — the driver is still
# in the kernel, just not separately loadable.
MODDIR="$WORKDIR/lib/modules/$KVER"
mkdir -p "$MODDIR"

# Copy one module file from MOD_SRC into MODDIR, decompressing Zstd/xz/gz
# on the way so ee-init's init_module (whole-image, plain ELF .ko only) can
# load it. Tolerates missing source files — modprobe's dep tree can
# reference stale entries on zombie/partial kernel installs, and we
# don't want the whole build to die for one missing dep.
copy_mod() {
    local src="$1"
    [ -z "$src" ] && return 0
    if [ ! -f "$src" ]; then
        echo "  skip (missing): $src"
        return 0
    fi
    local rel="${src#"$MOD_SRC/"}"
    local dst="$MODDIR/$rel"
    mkdir -p "$(dirname "$dst")"
    case "$src" in
        *.ko.zst) zstd -d -q -f -o "${dst%.zst}" "$src" || return 0 ;;
        *.ko.xz)  xz -d -c "$src" > "${dst%.xz}"       || return 0 ;;
        *.ko.gz)  gzip -d -c "$src" > "${dst%.gz}"     || return 0 ;;
        *)        cp --update=none "$src" "$dst"       || return 0 ;;
    esac
}

for top in $TARGET_INITRD_MODULES; do
    deps=$(modprobe --show-depends --set-version "$KVER" "$top" 2>&1 || true)
    if ! echo "$deps" | grep -q '^insmod'; then
        echo "  $top: not available as a module on $KVER (built-in or absent)"
        continue
    fi
    count=0
    # shellcheck disable=SC2034
    while read -r line; do
        src=$(echo "$line" | awk '/^insmod/ { print $2 }')
        [ -z "$src" ] && continue
        copy_mod "$src"
        count=$((count + 1))
    done <<<"$deps"
    echo "  $top: $count files processed"
done

# Regenerate modules.dep from the decompressed tree. depmod scans the
# files it finds and writes fresh entries, so the paths will reference
# plain .ko (matching what ee-init reads from modules.dep and loads).
depmod -b "$WORKDIR" "$KVER"

# Diagnostic: list what ended up in the initrd module tree so build
# logs show whether tdx-guest/nvme/etc. landed as .ko files or fell
# through to "built-in" status. If modules.dep is empty, ee-init's
# module load will no-op, which is fine as long as the corresponding
# drivers are compiled into the kernel.
echo "=== initrd module tree for $KVER ==="
find "$MODDIR" -type f -name '*.ko' 2>/dev/null | sort | sed "s|$MODDIR/||" || true
echo "=== modules.dep ==="
cat "$MODDIR/modules.dep" 2>/dev/null || echo "(missing)"
echo "==="

# Install the static ee-init binary as /init. It is a fully static musl ELF
# (no interpreter / libs), so nothing else is needed in the initrd userspace.
# dm-verity (veritysetup) is intentionally not shipped — no target sets
# roothash=; ee-init fails loudly if one ever does.
cp "$EE_INIT_BIN" "$WORKDIR/init"
chmod +x "$WORKDIR/init"

# Create the cpio archive
(cd "$WORKDIR" && find . | cpio -o -H newc 2>/dev/null) | gzip -9 > "$OUTFILE"

SIZE=$(du -h "$OUTFILE" | cut -f1)
echo "Initrd built: $OUTFILE ($SIZE)"
