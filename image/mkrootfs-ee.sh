#!/bin/bash
# Build an EasyEnclave-owned runtime rootfs.
#
# This intentionally does not install a distro base system. The initrd
# provides early userspace, networking/config merge, and the writable
# tmpfs mounts before switch_root. The rootfs only needs PID 1, mount
# points, and the small config files EE/workloads read after boot.
set -euo pipefail

OUT="${1:?Usage: mkrootfs-ee.sh <rootfs-dir> <easyenclave-bin>}"
EE_BIN="${2:?Usage: mkrootfs-ee.sh <rootfs-dir> <easyenclave-bin>}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

[ -x "$EE_BIN" ] || { echo "mkrootfs-ee: $EE_BIN is not executable"; exit 1; }

rm -rf "$OUT"

install -d -m 0755 \
    "$OUT"/{dev,proc,sys,run,tmp,root,home,mnt,opt,srv,media} \
    "$OUT"/{bin,sbin,usr/bin,usr/sbin,usr/local/bin,var/lib/easyenclave,etc/easyenclave,etc/containers}
chmod 1777 "$OUT/tmp"

install -D -m 0755 "$EE_BIN" "$OUT/usr/local/bin/easyenclave"
ln -s /usr/local/bin/easyenclave "$OUT/sbin/init"

cat > "$OUT/etc/passwd" <<'EOF'
root:x:0:0:root:/root:/sbin/nologin
nobody:x:65534:65534:nobody:/nonexistent:/sbin/nologin
EOF

cat > "$OUT/etc/group" <<'EOF'
root:x:0:
nobody:x:65534:
EOF

cat > "$OUT/etc/hosts" <<'EOF'
127.0.0.1 localhost
::1 localhost ip6-localhost ip6-loopback
EOF

cat > "$OUT/etc/hostname" <<'EOF'
easyenclave
EOF

cat > "$OUT/etc/fstab" <<'EOF'
# root is mounted by the initrd; runtime writable paths are tmpfs overlays.
EOF

# The initrd vendor DHCP stage writes /run/resolv.conf before switch_root.
ln -s /run/resolv.conf "$OUT/etc/resolv.conf"

# Podman smoke workload config. These files are tiny and keep the test
# workload independent from distro defaults.
install -D -m 0644 "$SCRIPT_DIR/ee.extra/etc/containers/policy.json" \
    "$OUT/etc/containers/policy.json"
install -D -m 0644 "$SCRIPT_DIR/ee.extra/etc/easyenclave/podman-smoke-containers.conf" \
    "$OUT/etc/easyenclave/podman-smoke-containers.conf"

echo "mkrootfs-ee: built $OUT"
du -sh "$OUT"
