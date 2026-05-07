# TODO

## TDX measurement tracking
Build-time: capture MRTD + RTMR values for each release artifact, target profile, and launch topology. Registration-time: verify agent quotes against expected measurements. Sealing: encrypt secrets to measured state.

## Remove BusyBox and initrd helper utilities
Move boot-time userspace into `easyenclave` itself so the final image and
initrd do not depend on BusyBox applets or `veritysetup`.

Current dependency split:

- The final rootfs installs `busybox-static` and applet symlinks through
  `image/mkosi.conf` and `image/mkosi.postinst.chroot`.
- The initrd builder copies BusyBox and symlinks applets for shell init,
  module loading, root discovery, networking, DHCP, metadata fetches, and
  config parsing.
- The initrd also copies `veritysetup` when available, but native dm-verity
  activation is required before the helper-utility removal is complete.

Migration order:

1. Remove BusyBox from the final rootfs first. Replace smoke-test workloads
   that use `sh -c` and `busybox httpd` with either a tiny static test
   workload or a dedicated `easyenclave` test mode.
2. Stop installing BusyBox applet symlinks in `mkosi.postinst.chroot`, then
   remove `busybox-static` from `mkosi.conf` once runtime smoke tests no
   longer depend on `/bin/busybox`.
3. Add an `easyenclave initrd` mode and copy the release binary into the
   initrd while the existing shell `/init` remains the boot authority.
4. Make dm-verity image generation explicit: create the hash metadata during
   image assembly, persist the root hash as an artifact, and pass the data
   device, hash device, and `roothash=` in each target's UKI cmdline.
5. Implement native dm-verity activation in `easyenclave initrd` using
   device-mapper ioctls. Create and resume the `verity-root` mapper device,
   then mount `/dev/mapper/verity-root` read-only.
6. Port the root strategy from `image/init-templates/ext4-label.sh` into
   Rust: mount `/proc`, `/sys`, and `/dev`; parse kernel cmdline; load the
   required storage and attestation modules; resolve `LABEL=`/`UUID=` roots;
   mount tmpfs overlays; write `/run/easyenclave/env`; move virtual
   filesystems; and `switch_root` into `/sbin/init`.
7. Port vendor stages into Rust. Preserve the current contracts for static IP
   overrides, DHCP, GCP `ee-config`, Azure `userData`/`customData`, qemu
   config disks, flat JSON compatibility, and `KEY=VALUE` env merging.
8. Remove BusyBox from `mkinitrd.sh` after Rust owns root setup and vendor
   setup.
9. Remove `veritysetup` from `mkinitrd.sh`. Completion means the initrd
   contains `easyenclave`, kernel modules, and only the files required by the
   kernel/module/root strategy.

## Canonical local launch workflow
The README now points at build artifacts, but the current working tree has no local launcher script. Decide whether to restore a QEMU/libvirt helper or document exact external launch commands for `gcp` qcow2 and `local-tdx` ISO, including config-disk handling.

## GPU passthrough image packaging
NVIDIA module and firmware staging is currently image-local and kernel-version specific. Profile-gate it, avoid hard-coded host kernel paths, and document attestation/measurement impact before treating GPU images as release artifacts.

## Workload restart policy
Process workloads don't auto-restart on crash. Add a configurable restart policy (always, on-failure, never) in `workload.rs` that supervises spawned children.

## Health check for workloads
Per-workload health check config (cmd, interval, retries). Mark workload unhealthy/restart on failure. Currently only tracks running/stopped/failed.
