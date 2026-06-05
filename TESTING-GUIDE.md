# CIBOS — Testing Guide (QEMU and USB)

This guide covers how to **build and boot the current CIBOS** on Ubuntu Linux,
both in QEMU (emulated, zero-risk) and on a real machine via USB. It states
exactly what works today and what does not yet, so you know what to expect.

> **Current state (what boots):** the `compute` and `performance` profiles build
> a complete, bootable x86-64 disk image and boot all the way to
> `CIBOS kernel: boot complete` — through the from-scratch bootloader, CIBIOS
> firmware, the CIBOS microkernel, and one run of the weighted-entropy scheduler.
> Output appears on **both** the serial console and the **VGA screen**.
>
> **What does not boot yet:** the `maximum-isolation` and `balanced` profiles
> (they need the no_std SPHINCS+ verifier, not done yet); the i686 (32-bit)
> image (two unrelated gaps, see §7); ARM/RISC-V USB images (those boot via
> device tree, not a BIOS image). Phones are **not** testable yet (§8).

---

## 0. Prerequisites (Ubuntu)

Install the toolchain and emulator once:

```sh
# Rust (stable) with the bare-metal target
curl https://sh.rustup.rs -sSf | sh -s -- -y
. "$HOME/.cargo/env"
rustup target add x86_64-unknown-none
rustup component add llvm-tools-preview

# GNU binutils (for the bootloader assembly) + QEMU + helpers
sudo apt-get update
sudo apt-get install -y build-essential qemu-system-x86 socat python3

# (Only needed if you ever build the i686 path — not required for x86-64)
rustup toolchain install nightly
rustup component add rust-src --toolchain nightly
```

Versions this was verified against: Rust 1.96, GNU binutils 2.42, QEMU 8.2.

Unpack the archive and enter the workspace:

```sh
tar -xzf cibos-FULL-sandbox.tar.gz
cd cibos-complete/cibos-workspace
```

---

## 1. Build the bootable image

One command builds everything (bootloader → CIBIOS firmware → CIBOS kernel →
assembled `.img`):

```sh
./build-bootimage.sh compute
# or:
./build-bootimage.sh performance
```

Result: `images/cibos-compute-x86_64.img` (~544 KiB). The script prints the disk
layout (Stage 1 at LBA 0, the layout descriptor at LBA 1, Stage 2, CIBIOS, then
the CIBOS image) and the CIBIOS entry address it read from the firmware ELF.

If you only want to (re)build the bootloader binaries by themselves:

```sh
./bootloader/build.sh        # -> bootloader/build/{stage1.bin,stage2-x86_64.bin,stage2-i686.bin}
```

---

## 2. Test in QEMU (recommended first — zero risk)

### 2a. The easy way (one command, captures serial + VGA)

```sh
./qemu-boot.sh compute
```

This builds the image if needed, boots it headless, prints the **serial** log,
then decodes and prints the **VGA text screen**. Optional args:
`./qemu-boot.sh <profile> [seconds] [mem_mib]` (defaults: 9 s, 128 MiB).

**Expected output (both serial and VGA show this):**

```
CIBIOS v0.1.0 starting
detected: 1 core(s), 127 MiB RAM at 0x100000
profile: X86_64 on Desktop, 1 logical context(s), SMT off
firmware profile: Lightweight
CIBOS image found (414128 bytes); booting
image verified (signature skipped), entry 0x1000000
components placed
handoff built; transferring control to CIBOS
CIBOS kernel: entry
CIBOS kernel: heap online (8388608 bytes)
CIBOS kernel: handoff accepted, 133692416 bytes usable across 1 region(s)
CIBOS kernel: init lane running
CIBOS kernel: scheduler idle after 1 poll(s)
CIBOS kernel: boot complete
```

If you see `CIBOS kernel: boot complete`, the entire from-scratch boot chain
worked end to end.

### 2b. Manual QEMU (to watch the actual VGA window)

To see the graphical VGA console in a window instead of a headless dump:

```sh
qemu-system-x86_64 \
  -drive format=raw,file=images/cibos-compute-x86_64.img \
  -m 128 \
  -serial stdio \
  -no-reboot
```

A QEMU window opens showing the white-on-black CIBOS boot text; the same text
streams to your terminal via serial. Close the window or Ctrl-C to stop.

### 2c. Manual QEMU, fully headless (serial only)

```sh
qemu-system-x86_64 \
  -drive format=raw,file=images/cibos-compute-x86_64.img \
  -m 128 -display none -serial stdio -no-reboot
```

### 2d. Dumping the VGA screen yourself (headless)

```sh
# start QEMU with a monitor socket
qemu-system-x86_64 -drive format=raw,file=images/cibos-compute-x86_64.img \
  -m 128 -display none -serial stdio -no-reboot \
  -monitor unix:/tmp/mon.sock,server,nowait &
sleep 9
# dump the 80x25 text buffer (0xB8000, 4000 bytes) and decode it
printf 'pmemsave 0xB8000 4000 "/tmp/vga.bin"\n' | socat - UNIX-CONNECT:/tmp/mon.sock
python3 -c "
d=open('/tmp/vga.bin','rb').read()
for r in range(25):
    row=''.join(chr(d[(r*80+c)*2]) if 32<=d[(r*80+c)*2]<127 else ' ' for c in range(80))
    if row.strip(): print(row.rstrip())
"
```

### Troubleshooting QEMU

* **Nothing on serial, immediate reboot loop** — drop `-no-reboot` and add
  `-d int,cpu_reset -D /tmp/qlog.txt`, then inspect `/tmp/qlog.txt` for the
  faulting instruction pointer.
* **`Cannot load x86-64 image, give a 32bit one`** — that error only happens
  with `-kernel`; do **not** use `-kernel`. Always boot the disk image with
  `-drive format=raw,file=...` (BIOS boots our MBR, exactly like a real machine).

---

## 3. Test on real hardware via USB

> ⚠️ Writing to a USB device is **destructive** and erases everything on it.
> Triple-check the device name. Test in QEMU first.

The image is a raw, directly-bootable disk image (a 512-byte MBR boot sector
with the `0xAA55` signature, followed by Stage 2, CIBIOS, and the CIBOS image).
You write the **whole image** to the **whole device** (not a partition).

### 3a. Identify the USB device

Plug in the USB stick, then:

```sh
lsblk -o NAME,SIZE,MODEL,TRAN     # find the USB disk, e.g. sdb (TRAN=usb)
```

Be certain: `sda` is usually your system disk. The USB is typically `sdb`,
`sdc`, etc. and shows `usb` in the TRAN column.

### 3b. Write with `dd` (the recommended way on Linux)

```sh
# unmount any auto-mounted partitions first
sudo umount /dev/sdX* 2>/dev/null

# write the image to the RAW device (note: of=/dev/sdX, NOT /dev/sdX1)
sudo dd if=images/cibos-compute-x86_64.img of=/dev/sdX bs=1M conv=fsync status=progress

sync
```

Replace `sdX` with your actual device (e.g. `sdb`). That's it — the stick is now
bootable.

### 3c. About Rufus / balenaEtcher / other tools

You asked about Rufus specifically. Notes:

* **Rufus is Windows-only.** On Ubuntu you do not need it — `dd` does the same
  job and is the standard tool. If you are preparing the stick from a Windows
  machine, Rufus works: choose the `.img`, select **"DD Image"** mode (not "ISO"
  mode), and write. Our image is a raw disk image, so DD mode is required.
* **balenaEtcher** (cross-platform, Linux/Mac/Windows GUI) also works: select the
  `.img`, select the USB device, Flash. It writes raw images correctly.
* **GNOME Disks** (built into Ubuntu): open Disks, select the USB device,
  hamburger menu → "Restore Disk Image…", choose the `.img`, Restore.

All of these do the same thing as `dd`: a byte-for-byte copy of the image onto
the device. Any of them is fine; `dd` is simplest on Ubuntu.

### 3d. Boot the target machine from USB

1. Insert the USB into the target PC.
2. Enter the firmware/BIOS boot menu (commonly **F12**, **F2**, **Esc**, or
   **Del** at power-on — varies by vendor).
3. **Enable Legacy/CSM/BIOS boot** (disable "UEFI-only" / enable "Legacy
   Support"). **CIBOS currently boots via legacy BIOS only — there is no UEFI
   loader yet** (see §6). On many machines you also disable Secure Boot.
4. Select the USB device from the boot menu.
5. You should see the same `CIBIOS v0.1.0 starting … CIBOS kernel: boot
   complete` text on the monitor (VGA console), and on a serial cable if you
   have one wired.

### 3e. What "booting" looks like / what to expect on hardware

* The screen shows the boot text and stops at `CIBOS kernel: boot complete`,
  then the machine is idle (the kernel runs its init lane and parks). This is the
  expected end state right now — there is no shell or GUI yet (§6).
* If the screen stays blank or the machine reboots immediately: confirm Legacy
  BIOS boot is enabled, Secure Boot is off, and that you wrote the image to the
  whole device (`/dev/sdX`, not a partition).

---

## 4. Live USB vs persistent vs installing to a disk

You asked about live/persistent/install. Here is the honest current picture:

* **Live USB:** Yes — this is exactly what §3 produces. The image runs entirely
  from RAM/firmware at boot; it does not need or touch any internal disk. It is
  "live" by nature.
* **Persistent storage:** **Not yet.** CIBOS does not yet have a storage/block
  driver wired into the boot path or a filesystem the kernel mounts at runtime,
  so there is nothing that persists between boots today. (The workspace has a
  `storage/` crate, but it is not yet a booted, writing filesystem — that comes
  with the storage/driver work on the roadmap.)
* **Installing to an internal disk / other media:** Writing the same `.img` to
  any disk (internal SATA/NVMe via `dd`, an SD card, etc.) makes that medium
  boot CIBOS the same way the USB does, because the image is a complete bootable
  disk. There is **no installer** (no partitioning, no copy-to-disk step) — you
  are simply writing the bootable image to whatever medium you want to boot from.
  That medium will boot straight into CIBOS, exactly like the USB.

So today: live boot from any medium you `dd` the image onto — yes. A persistent
data partition or a guided installer — not yet.

---

## 5. Quick reference — all commands in one place

```sh
# build
./build-bootimage.sh compute            # -> images/cibos-compute-x86_64.img

# test in QEMU (headless, prints serial + VGA)
./qemu-boot.sh compute

# test in QEMU (windowed, see the VGA console)
qemu-system-x86_64 -drive format=raw,file=images/cibos-compute-x86_64.img \
  -m 128 -serial stdio -no-reboot

# write to USB (DESTRUCTIVE — check the device!)
lsblk -o NAME,SIZE,MODEL,TRAN
sudo umount /dev/sdX* 2>/dev/null
sudo dd if=images/cibos-compute-x86_64.img of=/dev/sdX bs=1M conv=fsync status=progress
sync
```

---

## 6. What you will NOT see yet (so it's not a surprise)

These are not bugs — they are simply not built yet (see the roadmap doc):

* **No shell, no login prompt, no GUI.** The kernel boots, runs its init lane,
  prints `boot complete`, and idles. There is no interactive surface yet.
* **No keyboard/mouse interaction at the booted kernel.** Input drivers are not
  wired into the booted kernel yet.
* **No UEFI boot.** Legacy BIOS / CSM only. Modern machines must enable Legacy
  boot. (UEFI is a future loader.)
* **No networking, storage persistence, USB device stack, audio, or real
  display modes beyond VGA text.** These are roadmap items.
* **No memory protection between components yet** (the MMU/page-table isolation
  is on the roadmap; the bootloader sets up a flat identity map only).

---

## 7. The i686 (32-bit) image — why it's not buildable yet

`./build-bootimage.sh compute i686` will refuse, by design. Two unrelated gaps,
both outside the bootloader (the bootloader, the boot contract, mkbootimage, and
CIBIOS itself all build for i686):

1. `mkimage` cannot stamp the 32-bit `x86` arch tag, so CIBIOS would reject a
   32-bit CIBOS image as wrong-arch at boot.
2. `kernel-image`'s 32-bit x86 backend is incomplete (missing
   `arch::putc`/`init_serial`/`halt`; two type mismatches), so the kernel image
   does not compile for i686.

When both are fixed, i686 images can be produced the same way.

---

## 8. Can I test on a phone yet?

**No, not yet.** Phone bring-up is a later roadmap item and several
prerequisites are missing:

* The from-scratch bootloader here is a **legacy x86 BIOS** bootloader; phones
  are ARM (aarch64) and boot via a completely different mechanism (device tree /
  Android boot image / a vendor bootloader like a fastboot/`boot.img` flow), none
  of which is implemented yet.
* There is no ARM bootable image target, no mobile display/touch/connectivity
  drivers in the booted kernel, and no phone-flashing tooling.
* CIBIOS and the kernel *do* build for aarch64, but only as components booted via
  QEMU's `virt` device-tree path — not as a flashable phone image.

So: x86-64 PC via QEMU or USB is testable now; phones are not.

---

## 9. Safety reminders

* Always test in QEMU before touching real hardware — it is identical to the USB
  boot path from the firmware's point of view, and risk-free.
* `dd` to the wrong device will destroy that device's data. Confirm with `lsblk`
  every time.
* CIBOS does not write to any disk at runtime yet, so booting it on a machine
  does not modify the machine's internal drives — but the act of *writing the
  image to a USB/disk* with `dd` does erase that target medium.
