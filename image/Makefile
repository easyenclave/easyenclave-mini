# EasyEnclave VM Image Build
#
# Per-target image builds. Each target is a directory under targets/
# supplying a profile.env that drives modules, cmdline, root strategy,
# and output format.
#
# Boot chain (all targets):
#   UEFI firmware → ESP/EFI/BOOT/BOOTX64.EFI (UKI) → kernel + initrd
#                 → our init script → switch_root → easyenclave (PID 1)
#
# Workflow:
#   1. cargo build --release --target x86_64-unknown-linux-musl
#   2. mkrootfs-ee.sh → EE-owned rootfs tree     (shared)
#   3. copy host kernel                          (shared)
#   4. mkinitrd.sh <...> <profile>               (per-target modules + init template)
#   5. ukify → UKI                               (per-target cmdline)
#   6. mkimage.sh <profile>                      (per-target format: disk or iso)
#
# Prerequisites: cargo, rust musl target, musl-tools, busybox-static, e2fsprogs, mtools,
#   systemd-boot-efi (for the EFI stub), systemd-ukify, qemu-utils (qcow2)
#
# Usage:
#   make build                          # default TARGET=gcp
#   make build TARGET=azure             # Azure VHD for TDX CVMs
#   make build TARGET=local-tdx-qcow2   # libvirt backing-file qcow2
#   make clean                          # clean just the current TARGET's output
#   make clean-all                      # clean output/ entirely

REPO_ROOT := ..
EXTRA_BIN := ee.extra/usr/local/bin
EE_TARGET ?= x86_64-unknown-linux-musl
EE_BIN := $(REPO_ROOT)/target/$(EE_TARGET)/release/easyenclave
EFI_STUB := /usr/lib/systemd/boot/efi/linuxx64.efi.stub
KVER ?= $(shell uname -r)

TARGET ?= gcp
OUTPUT_DIR := output/$(TARGET)
PROFILE := targets/$(TARGET)/profile.env
ROOTFS_DIR := $(OUTPUT_DIR)/easyenclave.rootfs

.PHONY: build clean clean-all FORCE

build: $(EXTRA_BIN)/easyenclave
	@[ -f $(PROFILE) ] || { echo "Unknown TARGET=$(TARGET). Available: $$(ls targets/ 2>/dev/null)"; exit 1; }
	chmod +x mkrootfs-ee.sh mkinitrd.sh mkimage.sh assemble-disk.sh lib/mkesp.sh
	mkdir -p $(OUTPUT_DIR)
	@echo ""
	@echo "=== Building target: $(TARGET) ==="
	@cat $(PROFILE) | sed 's/^/  /'
	@echo ""
	# 1. Build the EE-owned rootfs as a directory.
	sudo rm -rf $(ROOTFS_DIR)
	sudo bash mkrootfs-ee.sh $(ROOTFS_DIR) $(EE_BIN)
	# 2. Per-target kernel cmdline.
	. $(PROFILE); echo "$$TARGET_CMDLINE" > $(OUTPUT_DIR)/easyenclave.cmdline
	# 3. Per-target initrd (profile-driven modules + init template).
	sudo bash mkinitrd.sh $(OUTPUT_DIR)/easyenclave.initrd $(KVER) $(PROFILE)
	# 4. Host kernel.
	sudo cp /boot/vmlinuz-$(KVER) $(OUTPUT_DIR)/easyenclave.vmlinuz
	sudo chmod 644 $(OUTPUT_DIR)/easyenclave.vmlinuz
	# 5. Build UKI.
	ukify build \
		--stub $(EFI_STUB) \
		--linux $(OUTPUT_DIR)/easyenclave.vmlinuz \
		--initrd $(OUTPUT_DIR)/easyenclave.initrd \
		--cmdline @$(OUTPUT_DIR)/easyenclave.cmdline \
		--output $(OUTPUT_DIR)/easyenclave.efi
	# 6. Per-target final image (GPT disk + derived formats).
	bash mkimage.sh $(PROFILE) $(OUTPUT_DIR)
	@echo ""
	@echo "=== Build complete ($(TARGET)) ==="
	@ls -lh $(OUTPUT_DIR)/ 2>/dev/null || true

$(EXTRA_BIN)/easyenclave: FORCE
	cargo build --manifest-path $(REPO_ROOT)/Cargo.toml --release --target $(EE_TARGET)
	mkdir -p $(EXTRA_BIN)
	cp $(EE_BIN) $(EXTRA_BIN)/easyenclave
	chmod 0755 $(EXTRA_BIN)/easyenclave

clean:
	# Only remove generated staging binaries inside ee.extra — the rest
	# of ee.extra remains tracked input for initrd/rootfs config files.
	sudo rm -f $(EXTRA_BIN)/easyenclave
	sudo rm -rf $(OUTPUT_DIR) easyenclave

clean-all:
	sudo rm -f $(EXTRA_BIN)/easyenclave
	sudo rm -rf output easyenclave
