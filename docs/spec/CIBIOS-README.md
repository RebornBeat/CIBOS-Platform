# CIBIOS: Complete Isolation Basic Input/Output System
**Boot Firmware Foundation — Establishing the Isolation Base**

## The Firmware Revolution: Isolation from Power-On

The Complete Isolation Basic Input/Output System (CIBIOS) is boot firmware that establishes HIP's isolation guarantees from the moment hardware powers on, creating the hardware foundation on which CIBOS builds its operating environment. CIBIOS implements HIP's architectural principles at the firmware layer, ensuring that the isolation properties CIBOS relies on are hardware-established before any operating system code executes.

Traditional firmware operates on trust-based models where each component must trust every other component, creating cascade failure scenarios. CIBIOS eliminates foundational vulnerabilities by implementing isolation principles at firmware level. Rather than depending on trust relationships, CIBIOS establishes hardware-enforced isolation boundaries, performs cryptographic or lightweight verification depending on the configured profile, and transfers control to CIBOS in a state where isolation is already active — not requested by the OS, but established by the firmware before the OS begins. CIBOS inherits isolation as a starting condition.

CIBIOS is designed to boot CIBOS. These two systems are architected together as a complete stack sharing a build system, feature flags, and architectural principles derived from HIP.

---

## HIP at the Firmware Layer

Before kernel execution, CIBIOS establishes:
- Memory isolation boundaries between regions
- Hardware-enforced component boundaries before drivers load
- Cryptographic verification chains where the profile requires them
- The lane memory regions that CIBOS will populate
- SMT configuration appropriate to the profile
- Hardware configuration record for CIBOS to inherit

The handoff mode is a shared build-time feature. CIBIOS and CIBOS are compiled together with matching handoff configuration, ensuring they agree on the protocol by construction.

---

## CIBIOS Profiles

CIBIOS provides two build-time profiles. Profiles are Rust feature flag configurations selected at build time. The binary is already configured when built.

### CIBIOS Standard Profile

**Purpose:** Systems needing boot-level cryptographic verification. Appropriate for multi-user systems, networked systems, any system where boot chain integrity is a security requirement.

**What Is Compiled In:**
- Cryptographic verification of the CIBOS kernel image before handoff
- Full boot component integrity verification chain
- Cryptographic entropy source for initialization randomness
- Hardware vendor features disabled unless explicitly added
- Complete isolation boundary establishment before kernel execution
- SMT disabled by default

**How Handoff Works:**

CIBIOS Standard computes a hash of the loaded CIBOS image, verifies it against a signature using the public key embedded in firmware at build time, and proceeds only if verification succeeds. If verification fails, boot stops with a diagnostic message. A CIBOS binary compiled with mismatched feature flags will have a different hash, causing boot to fail. This makes profile pairings self-enforcing.

**Pairs with CIBOS profiles:** Maximum Isolation, Balanced, Performance

### CIBIOS Lightweight Profile

**Purpose:** Systems where cryptographic boot verification overhead is inappropriate and physical security establishes the trust boundary. Appropriate for air-gapped single-user computation systems.

**What Is Compiled In:**
- Lightweight handshake transfer to CIBOS
- Minimal boot parameter verification
- Event-driven parallel hardware initialization
- Isolation boundary establishment before kernel execution
- SMT enabled by default

**What Is Not Compiled In:**
- Cryptographic signature verification of CIBOS image
- Measured boot sequence
- Attestation chain

**How Handoff Works:**

CIBIOS Lightweight loads the CIBOS kernel, establishes isolation boundaries, writes hardware configuration parameters to an agreed memory location, and transfers control to the CIBOS entry point. No signatures are verified. Trust is established by the physical environment: in a physically secured single-user system, the only entity loading software onto boot media is the trusted user.

**Why This Is Not a Security Compromise:** A cryptographic verification chain protects against an adversary who could modify the CIBOS binary without physical access to boot media. In a physically secured single-user air-gapped system, this adversary does not exist. Lightweight profile is correct threat modeling.

**Pairs with CIBOS profiles:** Compute, Performance (offline)

### Profile Pairing

CIBIOS and CIBOS are compiled together as a pair. The handoff mode ensures they agree on the protocol.

| CIBIOS Profile | Valid CIBOS Profiles | Reason |
|---|---|---|
| Standard | Maximum Isolation, Balanced, Performance | Cryptographic handoff requires matching key |
| Lightweight | Compute (Performance offline) | Lightweight handoff; security features absent by design |

When CIBIOS Standard attempts to boot a CIBOS Compute binary: CIBIOS computes hash of CIBOS image; hash won't match expected signature; boot fails. This is intentional.

```
PROFILE PAIRING:

┌─────────────────────────────────────────────────────────────────────────────┐
│                                                                             │
│  ┌──────────────────────────────────────────────────────────────────────┐   │
│  │                                                                      │   │
│  │  CIBIOS Standard          CIBIOS Lightweight                         │   │
│  │  ┌─────────────────┐      ┌─────────────────┐                        │   │
│  │  │ Cryptographic   │      │ Lightweight     │                        │   │
│  │  │ Handoff         │      │ Handoff         │                        │   │
│  │  │ SMT: Disabled   │      │ SMT: Enabled    │                        │   │
│  │  │ by default      │      │ by default      │                        │   │
│  │  └────────┬────────┘      └────────┬────────┘                        │   │
│  │           │                        │                                  │   │
│  │           ▼                        ▼                                  │   │
│  │  ┌─────────────────┐      ┌─────────────────┐                        │   │
│  │  │ CIBOS           │      │ CIBOS           │                        │   │
│  │  │ ─────────────   │      │ ─────────────   │                        │   │
│  │  │ Max Isolation   │      │ Compute         │                        │   │
│  │  │ Balanced        │      │ Performance     │ (offline only)         │   │
│  │  │ Performance     │      │                 │                        │   │
│  │  └─────────────────┘      └─────────────────┘                        │   │
│  │                                                                      │   │
│  │  ✓ Compatible             ✓ Compatible                               │   │
│  │  (Same handoff mode)      (Same handoff mode)                        │   │
│  │                                                                      │   │
│  │  ┌────────────────────────────────────────────────────────────────┐  │   │
│  │  │  CIBIOS Standard + CIBOS Compute = BOOT FAILURE                │  │   │
│  │  │  (Signature won't match — intentional self-enforcement)         │  │   │
│  │  └────────────────────────────────────────────────────────────────┘  │   │
│  │                                                                      │   │
│  └──────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## SMT Configuration at Boot

CIBIOS configures SMT at boot before transferring control to CIBOS. CIBOS inherits the SMT configuration established by CIBIOS.

| CIBIOS Profile | SMT Default | Reason |
|---|---|---|
| Standard | Disabled | Adversarial environments require hardware side-channel elimination |
| Lightweight | Enabled | Air-gapped environments maximize computation throughput |

SMT configuration is part of the hardware state CIBIOS establishes. The hardware configuration record includes SMT status and core counts so CIBOS can accurately report execution context count.

---

## Hardware Vendor Features: Philosophy and Security Considerations

### The Default: Native Isolation Only

By default, all CIBIOS profiles use native firmware-level isolation that operates independently of all vendor proprietary code, provides identical isolation guarantees across all platforms, can be fully audited as open-source code, and does not activate hardware vendor stacks below firmware level. This is the recommended configuration for all deployments.

### Intel VT-x

Intel Management Engine operates at a privilege level below firmware. Enabling VT-x activates Intel's virtualization stack which interacts with the Management Engine. Intel's virtualization components are proprietary and cannot be fully audited. The security model becomes partially dependent on Intel's undisclosed firmware.

### AMD SVM

AMD Platform Security Processor operates below firmware level. SVM activation interacts with AMD's secure processor stack.

### ARM TrustZone

ARM Trusted Firmware gains execution privileges above the operating system when TrustZone is activated. TrustOS is proprietary ARM firmware executing in the secure world.

### When Hardware Vendor Features May Be Appropriate

Hardware vendor features may be considered when performance requirements justify the vendor trust trade-off and users explicitly understand and accept the security implications. These features are never enabled by default and require explicit addition as feature flags:

- `hardware-vendor-vtx` — Intel VT-x (documented trust implications apply)
- `hardware-vendor-svm` — AMD SVM (documented trust implications apply)
- `hardware-vendor-trustzone` — ARM TrustZone (documented trust implications apply)

---

## Universal Hardware Support

### ARM Architecture

CIBIOS implements comprehensive ARM processor support enabling universal deployment across mobile devices, embedded systems, single-board computers, and ARM-based servers. Mobile firmware initialization includes power management and sensor controller initialization that maintains isolation while enabling necessary mobile functionality.

### x86 and x64 Architecture

Intel and AMD processor support provides compatibility across desktop computers, laptops, and servers. CIBIOS enables privacy-focused computing across all x86 hardware including older systems, extending hardware lifetime.

### RISC-V Open Architecture

RISC-V processor support ensures compatibility with emerging open-source processor architectures. Open-source hardware integration eliminates concerns about undisclosed surveillance features.

---

## Boot Sequence Design

### Event-Driven Parallel Initialization

CIBIOS initialization uses event-driven sequencing consistent with HIP's architectural principles. Each initialization step proceeds when its prerequisites signal completion, not after a fixed time delay. Steps without semantic ordering dependencies proceed in parallel.

```
CIBIOS INITIALIZATION FLOW:

┌─────────────────────────────────────────────────────────────────────────────┐
│                                                                             │
│  ┌──────────────────────────────────────────────────────────────────────┐   │
│  │                                                                      │   │
│  │  POWER ON                                                            │   │
│  │      │                                                               │   │
│  │      ▼                                                               │   │
│  │  ┌─────────────────┐                                                 │   │
│  │  │ CPU Init        │ (Architecture-specific assembly)                │   │
│  │  └────────┬────────┘                                                 │   │
│  │           │                                                          │   │
│  │           ▼                                                          │   │
│  │  ┌─────────────────┐                                                 │   │
│  │  │ BSS Zeroing     │                                                 │   │
│  │  └────────┬────────┘                                                 │   │
│  │           │                                                          │   │
│  │           ▼                                                          │   │
│  │  ┌─────────────────┐                                                 │   │
│  │  │ Serial Init     │ (Debug output available from here)              │   │
│  │  └────────┬────────┘                                                 │   │
│  │           │                                                          │   │
│  │           ▼                                                          │   │
│  │  ┌─────────────────┐                                                 │   │
│  │  │ Hardware RNG    │ (Check availability)                            │   │
│  │  │ Check           │                                                 │   │
│  │  └────────┬────────┘                                                 │   │
│  │           │                                                          │   │
│  │           ▼                                                          │   │
│  │  ┌─────────────────┐                                                 │   │
│  │  │ Hardware        │ (CPU, memory, storage, display)                 │   │
│  │  │ Detection       │                                                 │   │
│  │  └────────┬────────┘                                                 │   │
│  │           │                                                          │   │
│  │           ▼                                                          │   │
│  │  ┌─────────────────┐                                                 │   │
│  │  │ Memory          │ (Before any code uses memory)                   │   │
│  │  │ Isolation       │                                                 │   │
│  │  │ Boundaries      │                                                 │   │
│  │  └────────┬────────┘                                                 │   │
│  │           │                                                          │   │
│  │           ▼                                                          │   │
│  │  ┌─────────────────┐                                                 │   │
│  │  │ Lane Memory     │ (Reserve regions CIBOS will use for lanes)      │   │
│  │  │ Reservation     │                                                 │   │
│  │  └────────┬────────┘                                                 │   │
│  │           │                                                          │   │
│  │           ▼                                                          │   │
│  │  ┌─────────────────┐                                                 │   │
│  │  │ SMT Config      │ (Per profile: disabled=Standard, enabled=Light) │   │
│  │  └────────┬────────┘                                                 │   │
│  │           │                                                          │   │
│  │           ▼                                                          │   │
│  │  ┌─────────────────┐                                                 │   │
│  │  │ Boot Config     │ (Loading, first-boot detection)                 │   │
│  │  │ Loading         │                                                 │   │
│  │  └────────┬────────┘                                                 │   │
│  │           │                                                          │   │
│  │           ▼                                                          │   │
│  │  ┌─────────────────┐                                                 │   │
│  │  │ CIBOS Image     │                                                 │   │
│  │  │ Loading         │                                                 │   │
│  │  └────────┬────────┘                                                 │   │
│  │           │                                                          │   │
│  │           ▼                                                          │   │
│  │  ┌─────────────────┐     ┌─────────────────┐                        │   │
│  │  │ Standard        │     │ Lightweight     │                        │   │
│  │  │ Profile:        │     │ Profile:        │                        │   │
│  │  │ Compute SHA-256 │     │ No verification │                        │   │
│  │  │ Verify Ed25519  │     │ (physical trust)│                        │   │
│  │  │ signature       │     │                 │                        │   │
│  │  └────────┬────────┘     └────────┬────────┘                        │   │
│  │           │                       │                                  │   │
│  │           └───────────┬───────────┘                                  │   │
│  │                       │                                              │   │
│  │                       ▼                                              │   │
│  │           ┌─────────────────────┐                                    │   │
│  │           │ Handoff Data        │                                    │   │
│  │           │ Preparation         │                                    │   │
│  │           │ - Memory layout     │                                    │   │
│  │           │ - Isolation state   │                                    │   │
│  │           │ - SMT status        │                                    │   │
│  │           │ - Physical cores    │                                    │   │
│  │           │ - Logical cores     │                                    │   │
│  │           └──────────┬──────────┘                                    │   │
│  │                      │                                               │   │
│  │                      ▼                                               │   │
│  │           ┌─────────────────────┐                                    │   │
│  │           │ Transfer Control    │                                    │   │
│  │           │ to CIBOS Entry      │                                    │   │
│  │           │ Point — NEVER       │                                    │   │
│  │           │ RETURNS             │                                    │   │
│  │           └─────────────────────┘                                    │   │
│  │                                                                      │   │
│  └──────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### What CIBIOS Establishes Before Kernel Handoff

**Memory Isolation Boundaries:** Hardware memory management configured to enforce boundaries between all memory regions before CIBOS receives control.

**Lane Memory Regions:** CIBIOS reserves and initializes the memory regions CIBOS will use for lane execution contexts, isolated at hardware level.

**SMT Configuration:** Configured based on profile before CIBOS receives control.

**Hardware Configuration Record:** Complete record of hardware configuration, memory layout, initialized boundaries, and system capabilities (including SMT status, physical core count, logical core count) written to a known memory location. CIBOS reads this record rather than re-detecting hardware state.

### What CIBIOS Does Not Do During Boot

CIBIOS does not load or initialize user applications. CIBIOS does not configure network interfaces beyond the minimum for network boot. CIBIOS does not establish user profiles or authentication state. These are CIBOS responsibilities.

---

## What CIBIOS Protects Against and What It Cannot Prevent

### CIBIOS Protects Against

- Software-based attacks attempting to substitute an unauthorized CIBOS kernel (Standard profile)
- Compromise of OS components attempting to undermine isolation boundaries
- Software exploitation of initialization state before isolation is established

### CIBIOS Cannot Prevent

- Hardware-level surveillance mechanisms operating below firmware level (Intel ME, AMD PSP)
- Hardware vulnerabilities in the processor itself
- Physical tampering with hardware components after manufacturing
- Attack vectors requiring only physical access to storage media (Lightweight profile — addressed by trust model)

---

## The CIBIOS-CIBOS Build System Relationship

### Shared Feature Flags

Some feature flags apply to both CIBIOS and CIBOS simultaneously:

- `handoff-cryptographic` — CIBIOS verifies CIBOS signature; CIBOS provides its signature for verification
- `handoff-lightweight` — CIBIOS accepts CIBOS without cryptographic verification; CIBOS does not generate verification signature

These flags are defined at the workspace root level and automatically applied to both CIBIOS and CIBOS builds. Mismatched handoff flags are caught at compile time by the type system.

### Preventing Mismatched Binaries

**Standard profile:** A CIBIOS Standard binary computes the hash of whatever CIBOS binary is loaded and verifies against the expected signature. A CIBOS binary built with different feature configuration has a different hash. Boot fails with a verification error.

**Lightweight profile:** No signature verification occurs. CIBIOS Lightweight will transfer control to any valid CIBOS binary. Acceptable because the threat model assumes the user controls boot media.

---

## no_std: Bare-Metal Firmware Implementation

CIBIOS is bare-metal firmware. It requires:

```rust
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
```

CIBIOS provides a bump allocator (never frees; ~2MB heap — all memory reclaimed at CIBOS handoff). CIBIOS provides its own panic handler (writes to serial, halts). Hardware RNG is accessed directly:
- x86_64: RDRAND instruction (check CPUID leaf 1, ECX bit 30)
- ARM64: RNDR system register (check ID_AA64ISAR0_EL1)
- RISC-V: SEED CSR from Zkr extension

No async/await in CIBIOS. All functions synchronous. All errors returned as `Result<T, FirmwareError>`. `anyhow` requires `std` and is not used.

---

## Development Roadmap

**Phase 1 (Months 1-8):** Core firmware architecture — hardware initialization, isolation boundary establishment, SMT configuration, universal compatibility across all supported processor architectures. Both handoff modes implemented and validated.

**Phase 2 (Months 6-14):** Comprehensive testing of isolation boundary establishment across all supported hardware platforms. Performance optimization of boot sequences. Hardware vendor feature implementation for opt-in use.

**Phase 3 (Months 12-20):** Multi-platform validation across ARM, x86, x64, and RISC-V. Security validation of cryptographic verification chain. Compatibility testing with CIBOS profiles.

**Phase 4 (Months 18-24):** Open-source collaboration infrastructure. Production deployment validation. Documentation completion.

---

## Future Research: Non-Binary Substrates

CIBIOS's principles — event-driven initialization, isolation boundary establishment before OS execution, lightweight handoff — are substrate-agnostic. When non-binary hardware becomes practical, CIBIOS principles map directly to whatever initialization mechanisms that substrate requires. The semantic guarantees remain the design goal regardless of substrate.

---

**Project Repository:** github.com/cibos/complete-isolation-bios
**Supported Architectures:** ARM, x64, x86, RISC-V
**Profiles:** Standard (cryptographic handoff), Lightweight (lightweight handshake)
**License:** Privacy-focused open source with strong copyleft protections
