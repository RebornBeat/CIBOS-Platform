# Remaining Arc — Lattice integration, TCP, full per-arch build, WiFi

Bare-metal-FIRST and NO-DRIFT throughout. Every item targets a standardized
hardware/protocol interface; QEMU/cloud/real-hardware merely VERIFY it. Nothing
here simplifies away a canonical invariant (binary isolation, single selector,
no global locks, channel mutual-agreement, cell-grid Surface, custom async
runtime, human-auth gates a profile).

## PART A — Finish the networking arc (x86_64 first, then carried per-arch)

### A1 — Lattice connect()/Link over net_stack (remote UDP Links)
STATE: the Lattice surface (Gate/Link/Warden, syscalls 23-30) is loopback-backed
via the canonical Channel. net_stack (UDP transport over the NIC) exists and is
verified (DNS round-trip). The seam is built; the routing glue is not.
DO (no drift — the surface/APIs do NOT change; only a Link's backing transport):
  - Addressing model: a Gate may be LOCAL (loopback, today) or REMOTE (an
    IPv4:port endpoint). Extend Gate addressing so connect() to a remote Gate
    creates a Link whose byte transport is a net_stack UDP flow, not a Channel.
  - Map Link send/recv to udp_send_to / poll_udp for that flow (a (local_port,
    remote_ip, remote_port) tuple per Link). Keep the Warden check identical
    (denial still total). Datagram Links first (UDP); reliable/ordered Links wait
    for TCP (A2).
  - Apps + SDK Lattice calls UNCHANGED (NETWORKING.md guarantee): bind/connect/
    accept/send/recv/warden/probe behave the same; only reachability widens.
  - Verify in QEMU: two endpoints (guest <-> host, or guest <-> guest via socket
    networking) exchange bytes over a remote Link end to end.

### A2 — TCP in cibos-net (the reliability milestone)
From-scratch TCP state machine in cibos-net (no smoltcp): SYN/SYN-ACK/ACK,
sequence/ack tracking, retransmit, windowing, FIN/RST teardown. Backed by the
NetDevice; testable on LoopbackNic (host) + QEMU. Reliable/ordered Lattice Links
bind to TCP flows. Big, dedicated effort — its own plan when reached.

## PART B — FULL PER-ARCH BUILD FLOW (the arch sweep — what "fully built" means)

### B0 — Honest current state (verified this session)
- CIBIOS firmware: builds for ALL FOUR arches (x86_64, aarch64, riscv64gc,
  i686) — ELFs produced every archive.
- cibos-kernel core (scheduler, IPC, channels, gates, FS, net_device,
  cibos-net): ARCH-INDEPENDENT, builds for all targets.
- kernel-image: builds for aarch64/riscv64 but compiles only the MINIMAL core;
  the substantial surface is x86-GATED:
    x86_64 arch backend = 244 lines (GDT/IDT/PIC/PIT/paging/VGA/keyboard/ports) —
      COMPLETE.
    aarch64 backend = 38 lines (PL011 UART + halt) — serial only.
    riscv64 backend = 34 lines (SBI console + halt) — serial only.
    i686 = no dedicated backend (shares x86 paths via build-std).
  x86-ONLY modules: loader, ring3, keyboard, gui, timer, net_stack. So ring-3
  user space, ALL drivers (ATA, VGA, virtio-net, e1000), the interactive surface,
  and networking exist ONLY on x86_64 today.
- Boot paths differ: x86_64 + i686 -> bootable BIOS .img; aarch64/riscv64 ->
  QEMU -kernel (build-profile.sh).
"FULLY BUILT PER ARCH" is therefore a real, large effort — not a recompile.

### B1 — Per-arch kernel bring-up (do per arch: aarch64 -> riscv64 -> i686)
For EACH non-x86 arch, bring the kernel from "serial + halt" to a full runtime,
mirroring the x86_64 backend's responsibilities WITHOUT copying x86 mechanisms
(each arch has its own):
  1. Exception/trap vectors (aarch64: VBAR_EL1 vector table; riscv64: stvec +
     trap handler; i686: 32-bit IDT). Equivalent of the x86 IDT + fault reporter.
  2. Timer (aarch64: generic timer CNTP; riscv64: SBI timer / CLINT; i686: PIT) ->
     the same timer::now_ticks/wait_for surface the scheduler + hlt-wait need.
  3. Interrupt controller (aarch64: GICv2/v3; riscv64: PLIC; i686: 8259 PIC like
     x86) + the spurious/robustness handling we added for x86.
  4. MMU / page tables (aarch64: TTBR0/1, 4KB granule; riscv64: Sv39 satp; i686:
     32-bit paging) + an identity map for DMA, matching the x86 frame/identity
     model the drivers rely on.
  5. Console + input (aarch64/riscv64: UART RX for keys; i686: VGA + PS/2 like
     x86) -> the keyboard::has_key / read surface.
  6. Ring-3 / U-mode entry (aarch64: EL1->EL0 eret + SVC syscalls; riscv64:
     S->U mode + ecall; i686: ring0->ring3 iret + int 0x80) -> bring the
     selector-owned per-lane context save/resume + the syscall ABI over. The ABI
     is numeric + language-agnostic; each arch implements the trap entry/exit.
  7. Drivers per arch (B2 below).
Each step: build + (QEMU per that arch) boot-verify + tests + archive. NO drift:
the HIP invariants are identical across arches; only the CPU mechanisms differ.

### B2 — Per-arch drivers
Storage + display + NIC per arch, behind the SAME traits (BlockDevice, the
Surface, NetDevice) so the kernel/apps/Lattice are unchanged:
  - aarch64 (QEMU virt): virtio-mmio (block + net) — virtio again, but the
    MMIO transport (not PCI). A virtio-mmio NetDevice reuses our virtqueue logic.
  - riscv64 (QEMU virt): same virtio-mmio block + net.
  - i686: reuse the x86 PCI/ATA/VGA/virtio-net/e1000 drivers (same ISA family;
    32-bit pointer/paging differences only).
The NetDevice trait already abstracts this; a virtio-mmio backend is the main
new driver work for ARM/RISC-V.

### B3 — Per-arch platforms (CLI/GUI/Mobile/Server) once ring-3 + drivers exist
The platform runners (platform-cli/gui/mobile/server) + app crates are
arch-independent (they target the SDK/ABI). Once an arch has ring-3 + a console/
Surface + input, the platforms light up on it. Mobile is ARM-focused (touch +
Surface). Each platform verified per arch.

### B4 — Per-arch verification matrix (the "fully built per arch" bar)
For each arch x {boots, runs a .capp in ring-3, storage I/O, display/Surface,
keyboard/input, NIC TX+RX, Lattice loopback + (where wired) remote Link,
interactive login->shell}. A green matrix cell = that capability verified in
QEMU for that arch. The goal is a full green matrix, built honestly one cell at a
time, archived per increment.

## PART C — WiFi (login flow + per-platform connection flow)

### C0 — Scope + alignment (greenfield; no wifi code exists today)
WiFi is a NetDevice that requires ASSOCIATION before it carries frames: scan ->
select SSID -> authenticate (WPA2/WPA3: the 4-way handshake / SAE) -> associated
-> then it behaves like any NetDevice (DHCP, then the cibos-net stack runs over
it unchanged). Two distinct things the user means by "login flow":
  (a) HUMAN auth that GATES A PROFILE (existing F1 login->shell) — orthogonal to
      networking (canon invariant: human auth gates entry to a profile).
  (b) WiFi ASSOCIATION (joining a network) — a NETWORK operation, gated by the
      Warden + the active profile's policy. (b) is the new work.
Crypto: WPA2/WPA3 use established primitives (PBKDF2, AES, SAE/Dragonfly). Per
the dependency stance, cryptographic PRIMITIVES may use vetted crates (like the
pqcrypto-* precedent); the 802.11 STATE MACHINE + EAPOL logic is from-scratch OS
logic in a new cibos-wifi crate. NO drift.

### C1 — WiFi device + driver model
  - A WifiDevice trait (or NetDevice + an association control surface): scan(),
    associate(ssid, creds), status(), disassociate(); once associated, the
    frame TX/RX is the NetDevice path. Real chipsets (ath9k/iwlwifi/rtl) are huge;
    for bring-up + verification, target a virtio-/mac80211-style or a well-
    documented USB WiFi (later) — captured honestly as a large driver effort.
  - 802.11 management (scan/auth/assoc) + EAPOL 4-way handshake (WPA2) / SAE
    (WPA3) in a from-scratch cibos-wifi crate, using vetted crypto primitives.

### C2 — Connection flow PER PLATFORM (the UX surfaces differ; the core is shared)
The association core (cibos-wifi + the driver) is shared; each platform exposes
it differently, all behind the Lattice/Warden so policy is enforced uniformly:
  - CLI (terminal): a `wifi` app — `wifi scan`, `wifi connect <ssid>` (prompts
    for the passphrase, hidden input), `wifi status`, `wifi disconnect`. Stores
    known networks (encrypted) in CIBOSFS.
  - GUI (desktop): a network panel in settings/control-center — a scan list, a
    passphrase dialog on the Surface, connection status in the status bar.
  - Mobile (touch): a touch WiFi picker (tap SSID -> on-screen-keyboard
    passphrase -> connect), status in the status bar; known-network auto-join.
  - Server (headless): declarative config (a wifi profile in config) +
    `wifi-cli`/control-API association; no interactive prompt. For an AP/router
    role, the Lattice Gate routing + a future hostapd-equivalent (much later).
  - Across all: the human MUST be in an authenticated profile that PERMITS
    network changes (ties (a) and (b)); the Warden governs which Gates/SSIDs are
    allowed; credentials are stored encrypted; the flow is the SAME state machine
    underneath, only the I/O surface differs.

### C3 — WiFi verification
QEMU has limited native WiFi emulation, so WiFi is verified in stages: the
802.11/EAPOL state machine + crypto with unit tests + a simulated peer (host);
the driver against whatever emulation/passthrough is available; real hardware
last. Honest about which stage proves what — never claim hardware WiFi works off
a simulated handshake.

## SEQUENCING (recommended; archive after each landed increment)
A1 (remote UDP Links) -> B1 aarch64 bring-up -> B2 aarch64 virtio-mmio drivers ->
B3/B4 aarch64 platforms+matrix -> repeat B1-B4 for riscv64 -> i686 -> A2 (TCP) ->
C1-C3 (WiFi: crate + driver + per-platform flow + staged verification) ->
per-arch WiFi. Bare-metal-first; QEMU verifies; no invariant relaxed anywhere.

---

## A1 — DESIGN (no ABI change, no surface change) — being built now

KEY SEAM (verified in code): a Link is identified at the ABI by a `handle: u64`
(LinkSend/LinkRecv take a handle). The kernel's ChannelHandleTable maps
`(boundary, handle) -> Channel`. So a handle can resolve to EITHER a local
Channel-backed Link OR a remote UDP-backed Link, and link_send/link_recv dispatch
on which — with ZERO change to the syscall ABI or the SDK Lattice surface. Apps
call link_send(handle, ...) identically.

BUILD:
1. net_stack::RemoteLink — a UDP flow {local_port, remote_ip, remote_port} with
   send(&[u8]) and recv(&mut [u8]) over the stored NIC (udp_send_to / poll_udp).
2. ChannelHandleTable gains a parallel `remote_links: BTreeMap<(boundary,handle),
   RemoteLink>`; register_remote() mints a handle in the SAME handle space.
3. link_send/link_recv: resolve the Channel map first; if absent, the remote map;
   send/recv over UDP. Identical return semantics (bytes sent / Some(len)/None).
4. connect_remote(boundary, remote_ip, remote_port) mints a remote Link handle —
   the kernel-internal entry the addressing model (how an app NAMES a remote
   gate) will call. The app-facing addressing ABI is the follow-on; this
   increment proves the TRANSPORT branch end to end (a Link whose bytes traverse
   the NIC), mirroring how net_stack (transport) was built before its callers.

ALIGNMENT: the Gate/Link/Warden surface is unchanged; the Warden check stays
total; loopback remains the default; only a Link's backing transport widens to
the NIC. Matches NETWORKING.md ("only the fabric's backing transport changes").
Datagram (UDP) Links first; reliable/ordered Links await TCP (A2).
