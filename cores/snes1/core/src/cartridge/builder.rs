//! Single entry-point for assembling cartridges.
//!
//! [`CartridgeBuilder`] is what every client (the CLI, the WASM
//! bindings, the platform shell) goes through to turn a raw ROM
//! file, and an optional manifest, into a
//! [`Box<dyn SnesCartridge>`] that
//! [`crate::bus::SnesBus::install_cartridge`] can install
//! verbatim. The bus knows nothing about detection, manifests, or
//! SMC headers; that logic all lives here.
//!
//! ## Pipeline
//!
//! ```text
//! ROM bytes (+ optional manifest)
//!     |
//!     |-> strip_smc_if_present (drops 512-byte SMC prelude)
//!     |-> validate manifest preset string (even if forced overrides)
//!     |-> verify SHA-256 against manifest (default-on)
//!     |-> pad_to_power_of_two (LoROM/HiROM masks assume pow2)
//!     |-> attempt heuristic detection (for fallback metadata)
//!     |-> decide effective preset:
//!     |     forced > manifest > detector (threshold-gated)
//!     |-> resolve sram_size + fastrom:
//!     |     manifest > detected header > default (0 / false)
//!     |-> build base MapPlan via preset generator
//!     |-> append manifest slots + chip windows (if manifest present)
//!     |-> resolve chip instances (manifest chips[] + header DSP-1)
//!     `-> no chips => PlainCartridge; else re-home ROM/SRAM/plan behind
//!         a ChipSet built by the console factory => ComposableCartridge
//! ```
//!
//! ## Design notes
//!
//! - **SHA-256 default-on.** `manifest.rom.sha256` is a mandatory
//!   field in the schema; the default behaviour is to verify it.
//!   The check runs against the *stripped* bytes (no SMC prelude,
//!   no power-of-2 padding) because that's the canonical form
//!   shared by No-Intro / GoodSNES dumps. Call
//!   [`CartridgeBuilder::skip_sha256_check`] for fuzzing or
//!   tooling that intentionally produces non-canonical bytes. An
//!   empty `sha256` string in the manifest skips the check too,
//!   so manifests for in-development ROMs (no stable digest yet)
//!   keep working without an opt-out call.
//! - **`force_preset` precedence.** When both `force_preset` and
//!   `manifest.mapping.preset` are set, `force_preset` wins for
//!   the preset *choice*. The manifest's custom slots, chips,
//!   `sram_size`, and `fastrom` still apply, `force_preset` only
//!   overrides which stock generator produces the *base* plan.
//! - **`force_preset(Custom)` requires a manifest.** A custom plan
//!   with zero slots dispatches nothing; the builder rejects it
//!   loudly with [`BuildError::CustomRequiresManifest`].
//! - **Detection always runs.** Even when `force_preset` or a
//!   manifest is supplying the preset, the detector still runs
//!   (with a permissive threshold) so the parsed header is
//!   available as a fallback source of `sram_size` and `fastrom`.
//!   Only the *threshold gate* for picking the preset uses the
//!   user-configured value, `i32::MIN` for metadata fallback.
//! - **No `.srm` I/O.** [`CartridgeBuilder::with_sram`] accepts a
//!   pre-populated SRAM buffer for tests and other callers; a host
//!   saves and restores battery SRAM through
//!   `EmulatorCore::get_save_ram` / `set_save_ram` once the
//!   cartridge is installed.

use std::fmt::Write as _;
use std::ops::RangeInclusive;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use retro_core::cartridge::{ChipFactory, ChipSet};

use crate::cartridge::chip_support::{unsupported_chip, unsupported_preset};
use crate::cartridge::detect::{detect_preset_with_threshold, DEFAULT_DETECT_THRESHOLD};
use crate::cartridge::dsp1::{
    detect_dsp1_layout, is_dsp1_kind, CoprocessorMode, Dsp1Layout, DEFAULT_MASTER_CLOCK_HZ,
    DSP1_CHIP_ID,
};
use crate::cartridge::factory::{
    Cx4Config, Sa1Config, Snes1ChipFactory, Spc7110Config, SuperFxConfig,
};
use crate::cartridge::header::SnesHeader;
use crate::cartridge::manifest::{
    append_manifest_extras, parse_addr_range, parse_preset, CartridgeManifest, ManifestError,
    ManifestMapping, ManifestRom, SUPPORTED_MANIFEST_VERSION,
};
use crate::cartridge::map_plan::MapPlan;
use crate::cartridge::plain::PlainCartridge;
use crate::cartridge::presets::{
    cx4_preset, exhirom_preset, hirom_preset, lorom_preset, sa1_preset, sdd1_preset,
    spc7110_preset, superfx_preset, PresetKind,
};
use crate::cartridge::{
    ComposableCartridge, SnesCartridge, CX4_CHIP_ID, SA1_CHIP_ID, SDD1_CHIP_ID, SPC7110_CHIP_ID,
    SUPERFX_CHIP_ID,
};

/// One chip to instantiate into the cartridge's [`ChipSet`]: its `chip_id`,
/// the manifest `kind` the factory resolves, and the already-parsed bus
/// ranges it passively observes (empty for chips that observe nothing).
struct ChipInstance {
    id: u8,
    kind: String,
    observes: Vec<RangeInclusive<u32>>,
}

/// Fluent builder for [`SnesCartridge`] instances. See module docs
/// for the pipeline and design rationale.
///
/// `'a` borrows the ROM bytes: the builder never copies them
/// until [`CartridgeBuilder::build`] is called (which then owns
/// the stripped + padded bytes inside the resulting cartridge).
pub struct CartridgeBuilder<'a> {
    rom: &'a [u8],
    manifest: Option<CartridgeManifest>,
    forced_preset: Option<PresetKind>,
    sram: Option<Vec<u8>>,
    detect_threshold: i32,
    verify_sha256: bool,
    coprocessor_firmware: Option<Vec<u8>>,
    coprocessor_mode: CoprocessorMode,
}

impl<'a> CartridgeBuilder<'a> {
    /// Start a new builder over `rom`. Defaults: no manifest, no
    /// forced preset, no pre-populated SRAM, detect threshold =
    /// [`DEFAULT_DETECT_THRESHOLD`], SHA-256 verification on.
    pub fn new(rom: &'a [u8]) -> Self {
        Self {
            rom,
            manifest: None,
            forced_preset: None,
            sram: None,
            detect_threshold: DEFAULT_DETECT_THRESHOLD,
            verify_sha256: true,
            coprocessor_firmware: None,
            coprocessor_mode: CoprocessorMode::default(),
        }
    }

    /// Attach a manifest. The manifest's `preset`, `sram_size`,
    /// `fastrom`, custom slots, and chip declarations all feed
    /// into the final plan. If `force_preset` is also set, it
    /// wins for the preset *choice* but the rest of the manifest
    /// still applies.
    pub fn with_manifest(mut self, manifest: CartridgeManifest) -> Self {
        self.manifest = Some(manifest);
        self
    }

    /// Force a specific preset kind, overriding both the manifest's
    /// preset string and the detector's pick. Use this when the
    /// detector misbehaves (low-confidence header) or when the
    /// manifest's preset is intentionally aspirational. Combining
    /// `force_preset(Custom)` with no manifest is an error
    /// ([`BuildError::CustomRequiresManifest`]).
    pub fn force_preset(mut self, kind: PresetKind) -> Self {
        self.forced_preset = Some(kind);
        self
    }

    /// Pre-populate SRAM with the given buffer instead of starting
    /// from zeros. The buffer length becomes the cartridge's SRAM
    /// size verbatim, the resolved `sram_size` from the manifest
    /// / header is bypassed. Used by tests that need deterministic
    /// SRAM contents; savestates and `.srm` files restore SRAM through
    /// `SnesBus::restore_cartridge_sram` instead.
    pub fn with_sram(mut self, sram: Vec<u8>) -> Self {
        self.sram = Some(sram);
        self
    }

    /// Override the confidence floor for the detector. Defaults to
    /// [`DEFAULT_DETECT_THRESHOLD`]; set to `i32::MIN` for fuzzing
    /// or `i32::MAX` to force a manifest / force_preset path.
    pub fn detect_threshold(mut self, threshold: i32) -> Self {
        self.detect_threshold = threshold;
        self
    }

    /// Skip the SHA-256 verification that normally runs when a
    /// manifest is attached. Use for fuzzing, hacked test ROMs,
    /// or any workflow where the on-disk digest is intentionally
    /// out of date. An empty `sha256` string in the manifest
    /// already skips the check; this method is for non-empty
    /// digests that the caller intentionally wants to ignore.
    pub fn skip_sha256_check(mut self) -> Self {
        self.verify_sha256 = false;
        self
    }

    /// Supply the coprocessor firmware blob (e.g. an 8192-byte DSP-1
    /// dump). The chip factory feeds it to the DSP-1 LLE backend when a
    /// DSP-1 is detected, the mode permits LLE and the build carries it
    /// (the `nec-lle` feature; without it the firmware is ignored); byte
    /// order is auto-detected. With no firmware, a detected DSP-1 cart
    /// falls back to the firmware-free HLE (or, under forced LLE, the idle
    /// stub).
    pub fn with_coprocessor_firmware(mut self, firmware: Vec<u8>) -> Self {
        self.coprocessor_firmware = Some(firmware);
        self
    }

    /// Choose how a detected coprocessor is emulated
    /// ([`CoprocessorMode::Auto`] by default: LLE when firmware is
    /// present, otherwise the firmware-free HLE). [`CoprocessorMode::Hle`]
    /// forces the firmware-free HLE; [`CoprocessorMode::Lle`] forces LLE
    /// and falls back to the idle stub when no firmware is staged or the
    /// build has no LLE (`NEC_LLE_BUILT`).
    pub fn with_coprocessor_mode(mut self, mode: CoprocessorMode) -> Self {
        self.coprocessor_mode = mode;
        self
    }

    /// Consume the builder and produce a boxed cartridge. See
    /// [`BuildError`] for failure modes.
    pub fn build(self) -> Result<Box<dyn SnesCartridge>, BuildError> {
        // Move the consumed buffers out of `self` up front so the DSP
        // and plain-cartridge construction paths below can each take
        // ownership without partial-move headaches.
        let sram_override = self.sram;
        let coprocessor_firmware = self.coprocessor_firmware;
        let coprocessor_mode = self.coprocessor_mode;

        let stripped = strip_smc_if_present(self.rom);
        if stripped.is_empty() {
            return Err(BuildError::EmptyRom);
        }
        // The unpadded length is what the SPC7110's data-ROM address wraps
        // against (the real ROM size, not the power-of-two pad), so capture
        // it before padding.
        let real_rom_len = stripped.len();

        // Validate the manifest's preset string upfront. Even when
        // `force_preset` overrides the *choice*, a typo in the
        // manifest should fail loudly so it gets fixed.
        let manifest_kind = self
            .manifest
            .as_ref()
            .map(|m| parse_preset(&m.mapping.preset))
            .transpose()?;

        if self.verify_sha256 {
            if let Some(ref manifest) = self.manifest {
                let expected = manifest.rom.sha256.to_lowercase();
                if !expected.is_empty() {
                    let actual = sha256_lowercase(stripped);
                    if actual != expected {
                        return Err(BuildError::Sha256Mismatch { expected, actual });
                    }
                }
            }
        }

        let rom = pad_to_power_of_two(stripped.to_vec());

        // Always run detection with `i32::MIN` so the parsed header
        // is available as a fallback source of sram_size / fastrom
        // even when a manifest or `force_preset` is supplying the
        // preset itself. The user's threshold only gates the *use*
        // of detection for the preset choice (third match arm
        // below).
        let detected = detect_preset_with_threshold(&rom, i32::MIN);

        let effective_kind = match (self.forced_preset, manifest_kind, detected.as_ref()) {
            (Some(forced), _, _) => forced,
            (None, Some(kind), _) => kind,
            (None, None, Some(d)) if d.score >= self.detect_threshold => d.kind,
            (None, None, _) => {
                return Err(BuildError::NoMapping {
                    threshold: self.detect_threshold,
                });
            }
        };

        if effective_kind == PresetKind::Custom && self.manifest.is_none() {
            return Err(BuildError::CustomRequiresManifest);
        }

        let sram_size = self
            .manifest
            .as_ref()
            .map(|m| m.mapping.sram_size)
            .or_else(|| {
                detected
                    .as_ref()
                    .map(|d| resolved_ram_size(effective_kind, &d.header))
            })
            .unwrap_or(0);
        let fastrom = self
            .manifest
            .as_ref()
            .map(|m| m.mapping.fastrom)
            .or_else(|| detected.as_ref().map(|d| d.fastrom))
            .unwrap_or(false);

        // The RTC-4513 rides on the `$F9` SPC7110 header (Tengai Makyou Zero);
        // it gates the RTC register ports in the map + the clock in the chip.
        let has_spc7110_rtc = detected
            .as_ref()
            .map(|d| d.header.has_spc7110_rtc())
            .unwrap_or(false);

        // SA-1 BW-RAM persists only on `$34`/`$35` carts; `$32`/`$36` hold
        // volatile RAM. Derived from the header (the manifest carries only the
        // size), gating the chip's `battery()`.
        let sa1_has_battery = detected
            .as_ref()
            .map(|d| d.header.sa1_has_battery())
            .unwrap_or(false);

        // Super FX Game Pak RAM persists only on `$15` carts (Yoshi's Island);
        // Star Fox's 32 KiB is volatile scratch RAM. Derived from the header,
        // gating the chip's `battery()`.
        let superfx_has_battery = detected
            .as_ref()
            .map(|d| d.header.superfx_has_battery())
            .unwrap_or(false);

        // The Cx4 has no header battery signal, the real MMX2/X3 carts declare
        // no SRAM at all. RetroLab is a lab for ROMs, though, so a manifest that
        // declares `sram_size > 0` gets battery-backed SRAM (the SPC7110/SA-1
        // size-driven model): any declared Cx4 RAM persists.
        let cx4_has_battery = effective_kind == PresetKind::Cx4 && sram_size > 0;

        // A chip this build has no module for: refuse now, naming it, so the
        // host can pick another core, nothing is mapped or instantiated.
        if let Some(chip) = unsupported_preset(effective_kind) {
            return Err(BuildError::UnsupportedChip { chip });
        }

        let mut plan = match effective_kind {
            PresetKind::LoRom => lorom_preset(&rom, sram_size, fastrom),
            PresetKind::HiRom => hirom_preset(&rom, sram_size, fastrom),
            PresetKind::ExHiRom => exhirom_preset(&rom, sram_size, fastrom),
            PresetKind::Sdd1 => sdd1_preset(&rom, sram_size, fastrom),
            PresetKind::Spc7110 => {
                spc7110_preset(&rom, real_rom_len, sram_size, fastrom, has_spc7110_rtc)
            }
            PresetKind::Sa1 => sa1_preset(&rom, sram_size, fastrom),
            PresetKind::SuperFx => superfx_preset(&rom, sram_size),
            PresetKind::Cx4 => cx4_preset(&rom, sram_size),
            PresetKind::Custom => MapPlan::new("custom"),
        };

        if let Some(ref manifest) = self.manifest {
            append_manifest_extras(&mut plan, manifest)?;
        }

        // Resolve the DSP-1 register-window layout, if any. Two sources:
        //   1. An explicit manifest `chips[]` entry of kind "dsp1*", its
        //      register windows were already appended above, so we only need
        //      the layout for the DR/SR-select bit the factory hands the chip.
        //   2. The header rules of `detect_dsp1_layout`, we front-insert the
        //      register windows below so the first-wins resolver overrides the
        //      overlapping ROM mirror (the LoROM DSP windows shadow ROM banks).
        let manifest_dsp1 = self
            .manifest
            .as_ref()
            .map(|m| m.chips.iter().any(|c| is_dsp1_kind(&c.kind)))
            .unwrap_or(false);
        let header_layout = detected
            .as_ref()
            .and_then(|d| detect_dsp1_layout(&d.header, rom.len()));

        let dsp1_layout = if manifest_dsp1 {
            Some(header_layout.unwrap_or_else(|| layout_from_preset(effective_kind)))
        } else {
            header_layout
        };

        // Assemble the chips to instantiate: every manifest `chips[]` entry,
        // plus a header-detected DSP-1 the manifest did not already declare.
        // The header DSP-1 also front-inserts its two mirror windows so they
        // beat the overlapping ROM mirror, the delicate ordering the manifest
        // path can't express (see `detected_manifest`).
        let mut instances: Vec<ChipInstance> = Vec::new();
        if let Some(ref manifest) = self.manifest {
            for (i, chip) in manifest.chips.iter().enumerate() {
                let mut observes = Vec::with_capacity(chip.observes.len());
                for (j, range) in chip.observes.iter().enumerate() {
                    observes.push(parse_addr_range(
                        range,
                        &format!("chips[{i}].observes[{j}]"),
                    )?);
                }
                instances.push(ChipInstance {
                    id: chip.id,
                    kind: chip.kind.clone(),
                    observes,
                });
            }
        }
        if !manifest_dsp1 {
            if let Some(layout) = dsp1_layout {
                let [low, high] = layout.register_slots(DSP1_CHIP_ID);
                plan.slots.insert(0, high);
                plan.slots.insert(0, low);
                instances.push(ChipInstance {
                    id: DSP1_CHIP_ID,
                    kind: "dsp1".to_string(),
                    observes: Vec::new(),
                });
            }
        }

        // A detected S-DD1 cart declares its chip once. Unlike the DSP-1,
        // the dedicated `sdd1_preset` already owns the chip's register + MMC
        // windows, so nothing is front-inserted, the chip just needs a
        // ChipSet entry at its id, plus the `$4300-$437F` observer so it can
        // spy on the DMA registers and decompress armed transfers. A manifest
        // that names the chip itself (via `chips[]`) wins and suppresses this.
        let manifest_sdd1 = self
            .manifest
            .as_ref()
            .map(|m| m.chips.iter().any(|c| c.kind.eq_ignore_ascii_case("sdd1")))
            .unwrap_or(false);
        if effective_kind == PresetKind::Sdd1 && !manifest_sdd1 {
            // A one-window observe list really is a Vec of one range, the
            // chip model takes N windows; the S-DD1 needs exactly one.
            #[allow(clippy::single_range_in_vec_init)]
            instances.push(ChipInstance {
                id: SDD1_CHIP_ID,
                kind: "sdd1".to_string(),
                observes: vec![0x4300..=0x437F],
            });
        }

        // A detected SPC7110 cart declares its chip once, like the S-DD1: the
        // dedicated `spc7110_preset` already owns every chip window (registers,
        // MCU, DCU, SRAM), so nothing is front-inserted. Unlike the S-DD1 it
        // needs no bus observer, the decompressor is triggered by a direct
        // `$4806` write, not by snooping DMA. A manifest that names the chip
        // itself wins and suppresses this.
        let manifest_spc7110 = self
            .manifest
            .as_ref()
            .map(|m| {
                m.chips
                    .iter()
                    .any(|c| c.kind.eq_ignore_ascii_case("spc7110"))
            })
            .unwrap_or(false);
        if effective_kind == PresetKind::Spc7110 && !manifest_spc7110 {
            instances.push(ChipInstance {
                id: SPC7110_CHIP_ID,
                kind: "spc7110".to_string(),
                observes: Vec::new(),
            });
        }

        // A detected SA-1 cart declares its chip once, like the S-DD1/SPC7110:
        // the dedicated `sa1_preset` already owns every chip window (ROM via the
        // Super MMC, I-RAM, BW-RAM, registers), so nothing is front-inserted. It
        // needs no bus observer, the S-CPU reaches it directly through the
        // `$2200-23FF` carve-out. A manifest that names the chip itself wins and
        // suppresses this.
        let manifest_sa1 = self
            .manifest
            .as_ref()
            .map(|m| m.chips.iter().any(|c| c.kind.eq_ignore_ascii_case("sa1")))
            .unwrap_or(false);
        if effective_kind == PresetKind::Sa1 && !manifest_sa1 {
            instances.push(ChipInstance {
                id: SA1_CHIP_ID,
                kind: "sa1".to_string(),
                observes: Vec::new(),
            });
        }

        // A detected Super FX cart declares its chip once, like the SA-1: the
        // dedicated `superfx_preset` already owns the chip windows (registers +
        // Game Pak RAM), so nothing is front-inserted. It needs no bus observer:
        // the S-CPU reaches it directly through the promoted `$3000-34FF`
        // page. A manifest that names the chip itself wins and suppresses this.
        let manifest_superfx = self
            .manifest
            .as_ref()
            .map(|m| {
                m.chips
                    .iter()
                    .any(|c| c.kind.eq_ignore_ascii_case("superfx"))
            })
            .unwrap_or(false);
        if effective_kind == PresetKind::SuperFx && !manifest_superfx {
            instances.push(ChipInstance {
                id: SUPERFX_CHIP_ID,
                kind: "superfx".to_string(),
                observes: Vec::new(),
            });
        }

        // A detected Cx4 cart declares its chip once, like the Super FX: the
        // dedicated `cx4_preset` already owns the chip windows (ROM, MMIO + data
        // RAM, and any cart RAM), so nothing is front-inserted. It needs no bus
        // observer, the S-CPU reaches it directly through the promoted
        // `$6000-7FFF` page. A manifest that names the chip itself wins and
        // suppresses this.
        let manifest_cx4 = self
            .manifest
            .as_ref()
            .map(|m| m.chips.iter().any(|c| c.kind.eq_ignore_ascii_case("cx4")))
            .unwrap_or(false);
        if effective_kind == PresetKind::Cx4 && !manifest_cx4 {
            instances.push(ChipInstance {
                id: CX4_CHIP_ID,
                kind: "cx4".to_string(),
                observes: Vec::new(),
            });
        }

        // The Super FX and Cx4 own all cart RAM inside the chip (mapped via
        // ChipMmio slots), so the inner cartridge needs no SRAM buffer of its
        // own. Leaving it 0 keeps a non-battery cart (Star Fox, the real
        // MMX2/X3) from exposing a phantom all-zero `.srm`; a battery cart
        // (Yoshi's Island, a manifest-declared Cx4 SRAM) persists through the
        // chip's `battery()` instead.
        let inner_sram_size = if matches!(effective_kind, PresetKind::SuperFx | PresetKind::Cx4) {
            0
        } else {
            sram_size
        };
        let inner = match sram_override {
            Some(sram_bytes) => PlainCartridge::with_sram(rom, sram_bytes, plan),
            None => PlainCartridge::new(rom, inner_sram_size, plan),
        };

        // No chips => the plain cartridge is the whole story (zero overhead,
        // the common case).
        if instances.is_empty() {
            return Ok(Box::new(inner));
        }

        // At least one chip => re-home the ROM/SRAM/plan behind a `ChipSet`
        // and instantiate each chip through the console factory. Threading the
        // storage through `PlainCartridge::into_parts` keeps SRAM
        // initialisation byte-identical to the plain path; the ROM moves into
        // an `Arc<[u8]>` so a ROM-consuming chip (MMC / decompressor) can share
        // the image the bus reads.
        let (rom_bytes, sram, plan) = inner.into_parts();
        let rom_arc: Arc<[u8]> = Arc::from(rom_bytes.into_boxed_slice());
        let factory = Snes1ChipFactory {
            firmware: coprocessor_firmware,
            mode: coprocessor_mode,
            dsp1_layout,
            spc7110: Spc7110Config {
                real_rom_len,
                sram_size,
                has_rtc: has_spc7110_rtc,
            },
            sa1: Sa1Config {
                bwram_size: sram_size,
                has_battery: sa1_has_battery,
            },
            superfx: SuperFxConfig {
                ram_size: sram_size,
                has_battery: superfx_has_battery,
            },
            cx4: Cx4Config {
                ram_size: sram_size,
                has_battery: cx4_has_battery,
            },
        };
        let mut chips = ChipSet::default();
        for inst in instances {
            // A manifest can name a chip the detector did not: the same refusal.
            if let Some(chip) = unsupported_chip(&inst.kind) {
                return Err(BuildError::UnsupportedChip { chip });
            }
            let chip = factory
                .build(
                    &inst.kind,
                    inst.id,
                    rom_arc.clone(),
                    DEFAULT_MASTER_CLOCK_HZ,
                )
                .ok_or_else(|| BuildError::UnknownChip {
                    kind: inst.kind.clone(),
                })?;
            chips.insert(inst.id, chip);
            for range in inst.observes {
                chips.add_observer(range, inst.id);
            }
        }

        // A chip that owns the battery (the SPC7110's game SRAM) takes the
        // initial SRAM contents, the `with_sram` override or the zeroed
        // buffer, so the chip, not the cartridge's now-dead SRAM buffer,
        // holds the persistent save that the bus routes through it.
        if chips.has_battery() && !sram.is_empty() {
            chips.restore_battery(&sram);
        }

        let cart = ComposableCartridge::new(rom_arc, sram, plan, chips);
        Ok(Box::new(cart))
    }
}

/// Build the `cartridge-manifest-v1` that the builder's own
/// heuristics resolve for `rom`, with no override applied, the
/// "core detects and generates a manifest" entry point.
///
/// The returned manifest, fed straight back through
/// [`CartridgeBuilder::with_manifest`], reproduces the exact same
/// [`MapPlan`] the builder installs for the raw ROM. It captures the
/// detected preset, the header's SRAM size and FastROM flag, the
/// canonical (post-SMC-strip) SHA-256 + size, and the title/region.
///
/// Detection runs at the `i32::MIN` floor (the same fallback pass the
/// builder uses to read header metadata even when a manifest supplies
/// the preset), so a header that scores below the normal confidence
/// floor still contributes its SRAM/FastROM/title fields. When no
/// header parses at all the manifest falls back to a bare LoROM shell
/// the user can correct.
///
/// Coprocessors are deliberately *not* emitted as manifest `chips`.
/// The builder always re-runs [`detect_dsp1_layout`] on the header at
/// build time and front-inserts the DSP-1 register windows so they
/// beat the overlapping LoROM ROM banks under the first-wins
/// resolver. A manifest `chips[]` entry would instead append those
/// windows *after* the preset slots, where the ROM mirror shadows
/// them, so emitting one would break LoROM DSP-1 carts on re-apply.
/// The detected coprocessor is surfaced through `metadata.coprocessor`
/// for display only.
pub fn detected_manifest(rom: &[u8]) -> CartridgeManifest {
    let stripped = strip_smc_if_present(rom);
    let padded = pad_to_power_of_two(stripped.to_vec());
    let detected = detect_preset_with_threshold(&padded, i32::MIN);

    let preset = detected
        .as_ref()
        .map(|d| d.kind)
        .unwrap_or(PresetKind::LoRom);
    let sram_size = detected
        .as_ref()
        .map(|d| resolved_ram_size(preset, &d.header))
        .unwrap_or(0);
    let fastrom = detected.as_ref().map(|d| d.fastrom).unwrap_or(false);
    let title = detected
        .as_ref()
        .map(|d| d.header.title.trim_end().to_string())
        .filter(|t| !t.is_empty());
    let region = detected
        .as_ref()
        .map(|d| region_label(d.header.destination));

    let coprocessor = detected
        .as_ref()
        .and_then(|d| detect_dsp1_layout(&d.header, padded.len()))
        .map(dsp1_metadata);

    CartridgeManifest {
        schema: Some("cartridge-manifest-v1".to_string()),
        core: "snes1".to_string(),
        version: SUPPORTED_MANIFEST_VERSION.to_string(),
        rom: ManifestRom {
            sha256: sha256_lowercase(stripped),
            size: stripped.len() as u64,
            title,
            region,
        },
        mapping: ManifestMapping {
            preset: preset_to_str(preset).to_string(),
            fastrom,
            sram_size,
            slots: Vec::new(),
        },
        chips: Vec::new(),
        controllers: None,
        metadata: coprocessor,
    }
}

/// Stable lowercase token for a preset kind, matching the strings
/// [`parse_preset`] accepts so a detected manifest round-trips.
fn preset_to_str(kind: PresetKind) -> &'static str {
    match kind {
        PresetKind::LoRom => "lorom",
        PresetKind::HiRom => "hirom",
        PresetKind::ExHiRom => "exhirom",
        PresetKind::Sdd1 => "sdd1",
        PresetKind::Spc7110 => "spc7110",
        PresetKind::Sa1 => "sa1",
        PresetKind::SuperFx => "superfx",
        PresetKind::Cx4 => "cx4",
        PresetKind::Custom => "custom",
    }
}

/// Resolve the Game Pak / SRAM size for a detected cartridge, applying the
/// Super FX Star Fox quirk (no extended header -> forced 32 KiB). Every other
/// preset just reports the header's declared SRAM size.
fn resolved_ram_size(kind: PresetKind, header: &SnesHeader) -> usize {
    if kind == PresetKind::SuperFx {
        header.superfx_ram_size() as usize
    } else {
        header.sram_size_bytes() as usize
    }
}

/// Map the header destination byte (`+$29`) to a human region name.
/// Unknown codes fall back to their hex value so nothing is lost.
fn region_label(code: u8) -> String {
    match code {
        0x00 => "Japan",
        0x01 => "USA",
        0x02 => "Europe",
        0x03 => "Sweden/Scandinavia",
        0x04 => "Finland",
        0x05 => "Denmark",
        0x06 => "France",
        0x07 => "Netherlands",
        0x08 => "Spain",
        0x09 => "Germany",
        0x0A => "Italy",
        0x0B => "China",
        0x0D => "Korea",
        0x0E => "Common/International",
        0x0F => "Canada",
        0x10 => "Brazil",
        0x11 => "Australia",
        _ => return format!("0x{:02X}", code),
    }
    .to_string()
}

/// Informational coprocessor descriptor stored under
/// `metadata.coprocessor`. Not consumed by the builder (the DSP-1 is
/// re-derived from the header on every build); it exists so the
/// editor UIs can show "DSP-1 (HiROM window)" without re-detecting.
fn dsp1_metadata(layout: Dsp1Layout) -> serde_json::Value {
    serde_json::json!({
        "coprocessor": {
            "kind": "dsp1",
            "layout": format!("{:?}", layout),
        }
    })
}

/// Fallback [`Dsp1Layout`] when a manifest declares a DSP-1 chip but the
/// header heuristic can't classify it: HiROM-family presets get the
/// HiROM window, everything else the LoROM 1 MiB window.
fn layout_from_preset(kind: PresetKind) -> Dsp1Layout {
    match kind {
        PresetKind::HiRom | PresetKind::ExHiRom => Dsp1Layout::HiRom,
        _ => Dsp1Layout::LoRom1Mb,
    }
}

/// Failure modes for [`CartridgeBuilder::build`].
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// Manifest parsing / expansion failed. Carries the inner
    /// [`ManifestError`] verbatim so the caller can pinpoint the
    /// offending field.
    #[error("manifest error: {0}")]
    Manifest(#[from] ManifestError),
    /// The ROM was empty (either to begin with, or after stripping
    /// the SMC prelude).
    #[error("ROM is empty after SMC strip")]
    EmptyRom,
    /// No manifest, no `force_preset`, and the heuristic detector's
    /// best candidate did not cross the configured threshold.
    #[error("no mapping: detector score below threshold {threshold}")]
    NoMapping { threshold: i32 },
    /// `force_preset(Custom)` was set but no manifest was attached.
    /// A custom plan with zero slots wouldn't dispatch anything.
    #[error("force_preset(Custom) requires a manifest with slots")]
    CustomRequiresManifest,
    /// Manifest declared a SHA-256 digest that doesn't match the
    /// SHA-256 of the post-SMC-strip ROM bytes.
    #[error("manifest SHA-256 mismatch: expected {expected}, actual {actual}")]
    Sha256Mismatch { expected: String, actual: String },
    /// A `chips[]` entry (or a header-detected chip) named a `kind` the
    /// console factory doesn't know how to build. The chip's map slot is
    /// already in the plan, but no coprocessor can back it, so the build
    /// fails loudly instead of leaving a silently dead register window.
    #[error("unknown coprocessor chip kind: {kind}")]
    UnknownChip { kind: String },
    /// The cartridge needs a coprocessor this build has no module for (its
    /// `chip-*` Cargo feature is off). `chip` is the chip's display name
    /// (`"SA-1"`, `"Super FX"`, `"Cx4"`), so a host can name it and pick
    /// another core.
    #[error("unsupported chip: {chip} (not built into this SNES1 core)")]
    UnsupportedChip { chip: &'static str },
}

/// Strip the 512-byte SMC copier prelude when present (signalled
/// by `rom.len() % 1024 == 512`); pass through otherwise.
///
/// The 512-byte SMC header is a relic of the early-90s copiers
/// (Super Magicom, Pro Fighter Q) that prepended a metadata
/// block to dumps. Modern dumps from No-Intro / GoodSNES carry
/// no such prelude, so most ROMs hit the pass-through path. The
/// detector's `$81B0` / `$101B0` candidate offsets exist
/// precisely so legacy dumps still score correctly *before* the
/// strip, but once we've decided to strip, the canonical
/// digest matches the No-Intro form.
fn strip_smc_if_present(rom: &[u8]) -> &[u8] {
    if rom.len() >= 512 && rom.len() % 1024 == 512 {
        &rom[512..]
    } else {
        rom
    }
}

/// Pad `rom` with zero bytes until its length is the next power of
/// two. Already-power-of-two inputs are returned unchanged. Empty
/// inputs return empty (the caller is responsible for rejecting
/// them up front).
///
/// The LoROM / HiROM preset generators use a `mask = rom.len() - 1`
/// to fold over-large slot offsets back into the chip image; that
/// trick only works when `rom.len()` is a power of two. Padding
/// also makes `PlainCartridge::read_rom` return well-defined zero
/// bytes for the mirrored region of non-power-of-two ROMs like the
/// 6 MiB ExHiROM dumps (Tales of Phantasia among them) instead of
/// indexing past the end of the buffer.
fn pad_to_power_of_two(mut rom: Vec<u8>) -> Vec<u8> {
    if rom.is_empty() {
        return rom;
    }
    let len = rom.len();
    if len.is_power_of_two() {
        return rom;
    }
    let target = len.next_power_of_two();
    rom.resize(target, 0);
    rom
}

/// SHA-256 (lowercase hex) of a ROM after the SMC copier prelude is
/// stripped, the exact digest the manifest convention checks against
/// (see [`CartridgeBuilder::build`]). Savestates key on this so a state
/// is only loadable against the ROM it was captured from, regardless of
/// whether the on-disk image carried a 512-byte SMC header.
pub fn rom_sha256(rom: &[u8]) -> String {
    sha256_lowercase(strip_smc_if_present(rom))
}

/// SHA-256 of `bytes`, hex-encoded in lowercase. Centralized here
/// so [`BuildError::Sha256Mismatch`] only ever shows one canonical
/// format.
fn sha256_lowercase(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        write!(&mut out, "{:02x}", b).expect("writing to String never fails");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cartridge::header::{MAP_MODE_HIROM, MAP_MODE_LOROM};
    use crate::cartridge::parse_manifest;

    /// Build a ROM with a valid header at the requested offset and
    /// a sensible reset vector / title / checksum so the detector
    /// scores it well above threshold. Sram size code defaults to
    /// 0 (no SRAM).
    fn synth_rom(
        size: usize,
        offset: usize,
        map_mode: u8,
        rom_size_code: u8,
        sram_size_code: u8,
        reset_vector: u16,
    ) -> Vec<u8> {
        let mut rom = vec![0u8; size];

        let title = b"BUILDER TEST  ROM    ";
        rom[offset + 0x10..offset + 0x10 + 21].copy_from_slice(title);

        rom[offset + 0x25] = map_mode;
        rom[offset + 0x26] = 0x00;
        rom[offset + 0x27] = rom_size_code;
        rom[offset + 0x28] = sram_size_code;
        rom[offset + 0x29] = 0x01;
        rom[offset + 0x2A] = 0x01;
        rom[offset + 0x2B] = 0x00;

        let checksum: u16 = 0x1234;
        let complement = !checksum;
        rom[offset + 0x2C..offset + 0x2E].copy_from_slice(&complement.to_le_bytes());
        rom[offset + 0x2E..offset + 0x30].copy_from_slice(&checksum.to_le_bytes());

        rom[offset + 0x4C..offset + 0x4E].copy_from_slice(&reset_vector.to_le_bytes());

        rom
    }

    fn lorom_rom(size: usize) -> Vec<u8> {
        synth_rom(size, 0x7FB0, MAP_MODE_LOROM, 0x0A, 0x00, 0x8000)
    }

    fn hirom_rom(size: usize) -> Vec<u8> {
        synth_rom(size, 0xFFB0, MAP_MODE_HIROM, 0x0C, 0x00, 0x8000)
    }

    #[test]
    fn strip_smc_drops_512_bytes_when_size_mod_1024_is_512() {
        let rom = vec![0u8; 1024 + 512];
        let stripped = strip_smc_if_present(&rom);
        assert_eq!(stripped.len(), 1024);
    }

    #[test]
    fn strip_smc_passes_through_clean_dump() {
        let clean = vec![0u8; 1024];
        assert_eq!(strip_smc_if_present(&clean).len(), 1024);

        // Different-but-still-not-SMC sizes pass through.
        let odd = vec![0u8; 1024 + 256];
        assert_eq!(strip_smc_if_present(&odd).len(), 1024 + 256);
    }

    #[test]
    fn pad_to_power_of_two_already_power_returns_unchanged() {
        let rom = vec![0xAAu8; 1024];
        let padded = pad_to_power_of_two(rom.clone());
        assert_eq!(padded.len(), 1024);
        assert_eq!(padded, rom);
    }

    #[test]
    fn pad_to_power_of_two_3mib_becomes_4mib() {
        let mut rom = vec![0u8; 0x30_0000];
        rom[0x10_0000] = 0xAB;
        let padded = pad_to_power_of_two(rom);
        assert_eq!(padded.len(), 0x40_0000);
        // Pre-padded bytes survive.
        assert_eq!(padded[0x10_0000], 0xAB);
        // Padded region is zero.
        assert_eq!(padded[0x30_0000], 0);
        assert_eq!(padded[0x3F_FFFF], 0);
    }

    #[test]
    fn pad_to_power_of_two_zero_returns_zero() {
        let padded = pad_to_power_of_two(Vec::new());
        assert!(padded.is_empty());
    }

    #[test]
    fn sha256_lowercase_known_vector() {
        // RFC 6234 / NIST CAVS test vector for SHA-256 of "abc".
        assert_eq!(
            sha256_lowercase(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn build_lorom_with_detection_only() {
        let rom = lorom_rom(0x10_0000);
        let cart = CartridgeBuilder::new(&rom).build().expect("LoROM builds");
        assert_eq!(cart.map_plan().name, "lorom");
    }

    #[test]
    fn build_hirom_via_manifest() {
        let rom = hirom_rom(0x40_0000);
        let manifest_json = r#"{
            "core": "snes1",
            "version": "1.0",
            "rom": { "sha256": "", "size": 4194304 },
            "mapping": { "preset": "hirom", "fastrom": false, "sram_size": 0 }
        }"#;
        let manifest = parse_manifest(manifest_json).unwrap();
        let cart = CartridgeBuilder::new(&rom)
            .with_manifest(manifest)
            .build()
            .expect("HiROM via manifest builds");
        assert_eq!(cart.map_plan().name, "hirom");
    }

    #[test]
    fn build_force_preset_overrides_detection() {
        // ROM is a clean HiROM; we ask for LoROM anyway. The
        // detector would pick HiROM, but `force_preset` wins.
        let rom = hirom_rom(0x40_0000);
        let cart = CartridgeBuilder::new(&rom)
            .force_preset(PresetKind::LoRom)
            .build()
            .expect("forced LoROM builds");
        assert_eq!(cart.map_plan().name, "lorom");
    }

    /// `Box<dyn SnesCartridge>` does not impl `Debug`, so the
    /// usual `Result::unwrap_err` chain blows up on the `T:
    /// Debug` bound. Local helper unwraps the `Err` arm without
    /// needing the `Ok` arm to be printable.
    fn expect_err(result: Result<Box<dyn SnesCartridge>, BuildError>) -> BuildError {
        match result {
            Ok(_) => panic!("expected BuildError, got Ok"),
            Err(e) => e,
        }
    }

    #[test]
    fn build_force_preset_custom_without_manifest_errors() {
        let rom = lorom_rom(0x10_0000);
        let err = expect_err(
            CartridgeBuilder::new(&rom)
                .force_preset(PresetKind::Custom)
                .build(),
        );
        assert!(matches!(err, BuildError::CustomRequiresManifest));
    }

    #[test]
    fn build_no_manifest_no_force_garbage_rom_errors() {
        let rom = vec![0u8; 0x10_0000];
        let err = expect_err(CartridgeBuilder::new(&rom).build());
        match err {
            BuildError::NoMapping { threshold } => {
                assert_eq!(threshold, DEFAULT_DETECT_THRESHOLD);
            }
            other => panic!("expected NoMapping, got {:?}", other),
        }
    }

    #[test]
    fn build_sha256_mismatch_errors() {
        let rom = lorom_rom(0x10_0000);
        let manifest_json = r#"{
            "core": "snes1",
            "version": "1.0",
            "rom": {
                "sha256": "0000000000000000000000000000000000000000000000000000000000000000",
                "size": 1048576
            },
            "mapping": { "preset": "lorom" }
        }"#;
        let manifest = parse_manifest(manifest_json).unwrap();
        let err = expect_err(CartridgeBuilder::new(&rom).with_manifest(manifest).build());
        match err {
            BuildError::Sha256Mismatch { expected, actual } => {
                assert_eq!(
                    expected,
                    "0000000000000000000000000000000000000000000000000000000000000000"
                );
                // Hex-encoded SHA-256 is always 64 chars.
                assert_eq!(actual.len(), 64);
                assert_ne!(actual, expected);
            }
            other => panic!("expected Sha256Mismatch, got {:?}", other),
        }
    }

    #[test]
    fn build_skip_sha256_check_accepts_wrong_digest() {
        let rom = lorom_rom(0x10_0000);
        let manifest_json = r#"{
            "core": "snes1",
            "version": "1.0",
            "rom": { "sha256": "deadbeef", "size": 1048576 },
            "mapping": { "preset": "lorom" }
        }"#;
        let manifest = parse_manifest(manifest_json).unwrap();
        let cart = CartridgeBuilder::new(&rom)
            .with_manifest(manifest)
            .skip_sha256_check()
            .build()
            .expect("skipped SHA-256 still builds");
        assert_eq!(cart.map_plan().name, "lorom");
    }

    #[test]
    fn build_sram_size_resolution_manifest_wins_over_header() {
        // Header declares 8 KiB SRAM (code 0x03 = 1024 << 3).
        let rom = synth_rom(0x10_0000, 0x7FB0, MAP_MODE_LOROM, 0x0A, 0x03, 0x8000);
        let manifest_json = r#"{
            "core": "snes1",
            "version": "1.0",
            "rom": { "sha256": "", "size": 1048576 },
            "mapping": { "preset": "lorom", "sram_size": 32768 }
        }"#;
        let manifest = parse_manifest(manifest_json).unwrap();
        let mut cart = CartridgeBuilder::new(&rom)
            .with_manifest(manifest)
            .build()
            .expect("builds with manifest sram override");

        // 32 KiB SRAM = indices 0..0x7FFF. Writing at 0x7FFF must
        // round-trip; an 8 KiB buffer (the header value) would
        // silently drop the write and read back 0.
        cart.write_sram(0x7FFF, 0xCC);
        assert_eq!(cart.read_sram(0x7FFF), 0xCC);
    }

    #[test]
    fn build_with_sram_uses_provided_buffer() {
        let rom = lorom_rom(0x10_0000);
        let cart = CartridgeBuilder::new(&rom)
            .with_sram(vec![0xAB; 2048])
            .build()
            .expect("with_sram builds");
        assert_eq!(cart.read_sram(0), 0xAB);
        assert_eq!(cart.read_sram(2047), 0xAB);
        // One byte past the buffer end -> out-of-bounds -> 0.
        assert_eq!(cart.read_sram(2048), 0);
    }

    #[test]
    fn build_manifest_with_invalid_preset_errors_even_with_force_preset() {
        // The manifest's preset string is validated upfront so a
        // typo gets caught even when the caller forces a different
        // preset, broken manifests should never silently slip
        // through.
        let rom = lorom_rom(0x10_0000);
        let manifest_json = r#"{
            "core": "snes1",
            "version": "1.0",
            "rom": { "sha256": "", "size": 1048576 },
            "mapping": { "preset": "exlorom" }
        }"#;
        let manifest = parse_manifest(manifest_json).unwrap();
        let err = expect_err(
            CartridgeBuilder::new(&rom)
                .with_manifest(manifest)
                .force_preset(PresetKind::LoRom)
                .build(),
        );
        assert!(matches!(
            err,
            BuildError::Manifest(ManifestError::UnknownPreset(_))
        ));
    }

    // -- DSP-1 detection + wrapping --------------------------------------

    /// HiROM header (`map_mode $31`) + cartridge type `$05`, the Super
    /// Mario Kart signature that resolves to the HiROM DSP-1 window.
    fn smk_like_dsp1_rom() -> Vec<u8> {
        let mut rom = synth_rom(0x10_0000, 0xFFB0, 0x31, 0x0A, 0x00, 0x8000);
        rom[0xFFB0 + 0x26] = 0x05; // ROM + DSP coprocessor
        rom
    }

    #[cfg(feature = "nec-lle")]
    #[test]
    fn build_dsp1_hirom_with_firmware_is_lle() {
        let rom = smk_like_dsp1_rom();
        let cart = CartridgeBuilder::new(&rom)
            .with_coprocessor_firmware(vec![0u8; 8192])
            .build()
            .expect("DSP-1 HiROM builds");
        // Base preset is still HiROM (FastROM: map-mode $31 sets bit 4,
        // matching SMK's `sta $420d` MEMSEL enable); the DSP rides on top.
        assert_eq!(cart.map_plan().name, "hirom-fastrom");
        let label = cart.coprocessor_label().expect("DSP-1 present");
        assert!(label.contains("LLE"), "label = {label}");

        // The HiROM DSP window answers at $00:7000 (SR side, bit 12).
        let r = cart
            .map_plan()
            .resolve(0x00_7000)
            .expect("DSP window mapped");
        let slot = &cart.map_plan().slots[r.slot_index as usize];
        assert!(matches!(
            slot.target,
            crate::cartridge::MapTarget::ChipMmio {
                chip_id: DSP1_CHIP_ID,
                ..
            }
        ));
        // The slot mask reduces $7000 to exactly the SR-select bit.
        assert_eq!(r.target_offset, 0x1000);
    }

    #[cfg(not(feature = "nec-lle"))]
    #[test]
    fn build_dsp1_hirom_with_firmware_without_the_lle_build_is_hle() {
        // A build without the `nec-lle` feature ignores the firmware and
        // runs the HLE; the register window is mapped the same.
        let rom = smk_like_dsp1_rom();
        let cart = CartridgeBuilder::new(&rom)
            .with_coprocessor_firmware(vec![0u8; 8192])
            .build()
            .expect("DSP-1 HiROM builds");
        let label = cart.coprocessor_label().expect("DSP-1 present");
        assert!(label.contains("HLE"), "label = {label}");
        let r = cart
            .map_plan()
            .resolve(0x00_7000)
            .expect("DSP window mapped");
        assert_eq!(r.target_offset, 0x1000);
    }

    #[test]
    fn build_dsp1_hirom_without_firmware_is_hle() {
        // Auto (default) with no firmware installs the firmware-free HLE
        // rather than the idle stub, so DSP-1 games run firmware-free.
        let rom = smk_like_dsp1_rom();
        let cart = CartridgeBuilder::new(&rom)
            .build()
            .expect("HLE DSP-1 builds");
        let label = cart.coprocessor_label().expect("DSP-1 present");
        assert!(label.contains("HLE"), "label = {label}");
    }

    #[test]
    fn build_dsp1_hle_mode_forces_hle_even_with_firmware() {
        // Forced HLE ignores staged firmware and runs the HLE backend.
        let rom = smk_like_dsp1_rom();
        let cart = CartridgeBuilder::new(&rom)
            .with_coprocessor_firmware(vec![0u8; 8192])
            .with_coprocessor_mode(CoprocessorMode::Hle)
            .build()
            .expect("HLE DSP-1 builds");
        let label = cart.coprocessor_label().expect("DSP-1 present");
        assert!(label.contains("HLE"), "label = {label}");
    }

    #[test]
    fn build_dsp1_forced_lle_without_firmware_is_idle_stub() {
        // Forced LLE with no firmware surfaces the idle stub (the caller
        // explicitly wanted low-level emulation; don't silently swap HLE).
        let rom = smk_like_dsp1_rom();
        let cart = CartridgeBuilder::new(&rom)
            .with_coprocessor_mode(CoprocessorMode::Lle)
            .build()
            .expect("idle DSP-1 builds");
        let label = cart.coprocessor_label().expect("DSP-1 present");
        assert!(label.contains("idle"), "label = {label}");
        // Without the LLE in the build, the stub's label says so.
        assert_eq!(
            label.contains("not built"),
            !crate::cartridge::NEC_LLE_BUILT,
            "label = {label}"
        );
    }

    #[test]
    fn build_dsp1_lorom_window_precedes_rom_mirror() {
        // LoROM DSP-1 (map $20, type $03): the $20-$3F:8000-FFFF window
        // overlaps the LoROM ROM slot, so the DSP slot must be inserted
        // ahead of it for the first-wins resolver to pick the chip.
        let mut rom = synth_rom(0x8_0000, 0x7FB0, 0x20, 0x09, 0x00, 0x8000);
        rom[0x7FB0 + 0x26] = 0x03;
        let cart = CartridgeBuilder::new(&rom)
            .with_coprocessor_firmware(vec![0u8; 8192])
            .build()
            .expect("LoROM DSP-1 builds");
        let r = cart.map_plan().resolve(0x20_8000).expect("mapped");
        let slot = &cart.map_plan().slots[r.slot_index as usize];
        assert!(
            matches!(slot.target, crate::cartridge::MapTarget::ChipMmio { .. }),
            "DSP window must win over the ROM mirror at $20:8000"
        );
    }

    #[test]
    fn build_plain_cart_reports_no_coprocessor() {
        let rom = lorom_rom(0x10_0000);
        let cart = CartridgeBuilder::new(&rom).build().unwrap();
        assert!(cart.coprocessor_label().is_none());
    }

    // -- S-DD1 detection + build -----------------------------------------

    /// Star-Ocean-shaped ROM: LoROM header at $7FB0 but the
    /// `(cartridge_type, map_mode)` identifier is `$4532`, plus 8 KiB SRAM.
    fn star_ocean_like_rom(size: usize) -> Vec<u8> {
        // map_mode $32 (FastROM bit set), sram code $03 = 8 KiB.
        let mut rom = synth_rom(size, 0x7FB0, 0x32, 0x0C, 0x03, 0x8000);
        rom[0x7FB0 + 0x26] = 0x45; // cartridge_type -> identifier $4532
        rom
    }

    #[test]
    fn build_sdd1_from_detection() {
        let rom = star_ocean_like_rom(0x40_0000);
        let cart = CartridgeBuilder::new(&rom).build().expect("S-DD1 builds");
        // map_mode $32 sets the FastROM bit -> the fast plan variant.
        assert_eq!(cart.map_plan().name, "sdd1-fastrom");
        assert_eq!(cart.coprocessor_label(), Some("S-DD1"));

        // The MMC window is a ChipMmio slot over $C0-$FF (base 0).
        let r = cart
            .map_plan()
            .resolve(0xC0_0000)
            .expect("MMC window mapped");
        let slot = &cart.map_plan().slots[r.slot_index as usize];
        assert!(matches!(
            slot.target,
            crate::cartridge::MapTarget::ChipMmio {
                chip_id: SDD1_CHIP_ID,
                base: 0
            }
        ));

        // The register window resolves to the chip at the sentinel base.
        let r = cart
            .map_plan()
            .resolve(0x00_4804)
            .expect("register window mapped");
        let slot = &cart.map_plan().slots[r.slot_index as usize];
        assert!(matches!(
            slot.target,
            crate::cartridge::MapTarget::ChipMmio {
                chip_id: SDD1_CHIP_ID,
                base: crate::cartridge::SDD1_REG_WINDOW_BASE
            }
        ));
    }

    #[test]
    fn build_sdd1_registers_dma_observer() {
        // The detected S-DD1 chip registers the `$4300-$437F` observer so the
        // bus forwards DMA-register writes to it. Without it the chip could
        // never learn a transfer's source address or size, and would never
        // decompress.
        let rom = star_ocean_like_rom(0x40_0000);
        let cart = CartridgeBuilder::new(&rom).build().expect("S-DD1 builds");
        assert!(
            cart.observes_bus(),
            "a detected S-DD1 cart must observe the DMA registers"
        );

        // The observer survives the detected-manifest round-trip: the chip is
        // re-derived from `preset: "sdd1"`, not from a manifest chips[] entry.
        let manifest = detected_manifest(&rom);
        let json = serde_json::to_string(&manifest).expect("serializes");
        let parsed = parse_manifest(&json).expect("re-parses");
        let cart = CartridgeBuilder::new(&rom)
            .with_manifest(parsed)
            .build()
            .expect("rebuild from detected S-DD1 manifest");
        assert!(
            cart.observes_bus(),
            "the DMA observer must survive the manifest round-trip"
        );
    }

    #[test]
    fn detected_manifest_round_trips_sdd1() {
        // The exporter emits `preset: "sdd1"` with no chips[]; re-applying
        // reproduces the S-DD1 map and a live chip (the dedicated preset
        // owns the windows, so no front-insertion is needed).
        let rom = star_ocean_like_rom(0x40_0000);
        let manifest = detected_manifest(&rom);
        assert_eq!(manifest.mapping.preset, "sdd1");
        assert!(manifest.chips.is_empty(), "S-DD1 stays preset-derived");
        assert!(manifest.metadata.is_none(), "no DSP-1 metadata for S-DD1");

        let json = serde_json::to_string(&manifest).expect("serializes");
        let parsed = parse_manifest(&json).expect("re-parses");
        let cart = CartridgeBuilder::new(&rom)
            .with_manifest(parsed)
            .build()
            .expect("rebuild from detected S-DD1 manifest");
        assert_eq!(cart.coprocessor_label(), Some("S-DD1"));
        let r = cart
            .map_plan()
            .resolve(0xD0_0000)
            .expect("MMC window still mapped");
        let slot = &cart.map_plan().slots[r.slot_index as usize];
        assert!(matches!(
            slot.target,
            crate::cartridge::MapTarget::ChipMmio {
                chip_id: SDD1_CHIP_ID,
                base: 0
            }
        ));
    }

    // -- SA-1 detection + build + BW-RAM `.srm` bridge -------------------

    /// Super-Mario-RPG-shaped ROM: LoROM header at $7FB0 with map_mode $23 and
    /// cartridge_type $35 (SA-1 with battery-backed BW-RAM). `ram_code` is the
    /// header BW-RAM size code (`$05` = 32 KiB on the real SMRPG).
    fn sa1_like_rom(size: usize, cartridge_type: u8, ram_code: u8) -> Vec<u8> {
        let mut rom = synth_rom(size, 0x7FB0, 0x23, 0x0C, ram_code, 0x8000);
        rom[0x7FB0 + 0x26] = cartridge_type; // SA-1 sub-family
        rom
    }

    /// A build without the chip refuses the cartridge at once, naming it.
    #[cfg(not(feature = "chip-sa1"))]
    #[test]
    fn sa1_cart_is_refused_when_the_chip_is_not_built() {
        let rom = sa1_like_rom(0x40_0000, 0x35, 0x05);
        let err = CartridgeBuilder::new(&rom).build().err().expect("refused");
        assert!(
            matches!(err, BuildError::UnsupportedChip { chip: "SA-1" }),
            "{err}"
        );
        assert!(err.to_string().contains("SA-1"), "{err}");
    }

    #[cfg(feature = "chip-sa1")]
    #[test]
    fn build_sa1_from_detection() {
        let rom = sa1_like_rom(0x40_0000, 0x35, 0x05);
        let cart = CartridgeBuilder::new(&rom).build().expect("SA-1 builds");
        assert_eq!(cart.coprocessor_label(), Some("SA-1"));

        // The Super MMC owns ROM: the $8000-$FFFF window routes to the chip
        // (a ChipMmio slot), not a plain Rom target.
        let r = cart
            .map_plan()
            .resolve(0x00_8000)
            .expect("ROM window mapped");
        let slot = &cart.map_plan().slots[r.slot_index as usize];
        assert!(matches!(
            slot.target,
            crate::cartridge::MapTarget::ChipMmio {
                chip_id: SA1_CHIP_ID,
                ..
            }
        ));

        // The register file ($2200-$23FF) resolves to the chip so the
        // B-bus carve-out reaches it.
        let r = cart
            .map_plan()
            .resolve(0x00_2220)
            .expect("MMIO window mapped");
        let slot = &cart.map_plan().slots[r.slot_index as usize];
        assert!(matches!(
            slot.target,
            crate::cartridge::MapTarget::ChipMmio {
                chip_id: SA1_CHIP_ID,
                ..
            }
        ));
    }

    #[cfg(feature = "chip-sa1")]
    #[test]
    fn sa1_bwram_persists_through_srm_bridge() {
        // A `$35` SA-1 cart owns battery-backed BW-RAM. The composable
        // cartridge must aggregate it into `.srm` (via `ChipSet::battery`)
        // exactly like the SPC7110's game SRAM, proving the generic bridge
        // covers the SA-1 with no cartridge-side special-casing.
        let rom = sa1_like_rom(0x40_0000, 0x35, 0x05);
        let mut cart = CartridgeBuilder::new(&rom).build().expect("SA-1 builds");
        assert_eq!(cart.sram().len(), 0x8000, "BW-RAM size code $05 = 32 KiB");

        // A write through the SA-1's linear BW-RAM window surfaces in `.srm`.
        let base = crate::cartridge::SA1_BWRAM_LINEAR_BASE;
        cart.write_chip(SA1_CHIP_ID, base + 0x100, 0xAB, 0);
        cart.write_chip(SA1_CHIP_ID, base + 0x7FFF, 0xCD, 0);
        assert_eq!(cart.sram()[0x100], 0xAB, "BW-RAM write reaches `.srm`");
        assert_eq!(cart.sram()[0x7FFF], 0xCD);

        // Loading a `.srm` routes back into the chip's BW-RAM, readable again
        // through the window, the full save -> restore round-trip.
        let mut srm = vec![0u8; 0x8000];
        srm[0x40] = 0x5A;
        srm[0x7FFE] = 0x42;
        cart.restore_sram(&srm);
        assert_eq!(cart.read_chip(SA1_CHIP_ID, base + 0x40, 0), 0x5A);
        assert_eq!(cart.read_chip(SA1_CHIP_ID, base + 0x7FFE, 0), 0x42);
        assert_eq!(cart.sram()[0x40], 0x5A, "restored bytes surface in `.srm`");
    }

    #[cfg(feature = "chip-sa1")]
    #[test]
    fn sa1_without_battery_keeps_bwram_out_of_srm() {
        // A `$36` SA-1 cart has BW-RAM but no battery: the chip's `battery()`
        // returns `None`, so a BW-RAM write must NOT leak into the `.srm` the
        // host would persist (the cartridge falls back to its own buffer).
        let rom = sa1_like_rom(0x40_0000, 0x36, 0x05);
        let mut cart = CartridgeBuilder::new(&rom).build().expect("SA-1 builds");
        assert_eq!(cart.coprocessor_label(), Some("SA-1"));

        let base = crate::cartridge::SA1_BWRAM_LINEAR_BASE;
        cart.write_chip(SA1_CHIP_ID, base, 0x99, 0);
        // The BW-RAM still works as RAM...
        assert_eq!(cart.read_chip(SA1_CHIP_ID, base, 0), 0x99);
        // ...but the write does not surface in `.srm` (no battery to persist it).
        assert_eq!(
            cart.sram().first().copied(),
            Some(0),
            "no-battery BW-RAM must stay out of `.srm`"
        );
    }

    // -- Cx4 detection + build + manifest SRAM battery bridge ------------

    /// Mega-Man-X-shaped ROM: LoROM header at $7FB0 with map_mode $20 (same as
    /// stock LoROM) and cartridge_type $F3 (Cx4). `size` is the padded length.
    fn cx4_like_rom(size: usize) -> Vec<u8> {
        let mut rom = synth_rom(size, 0x7FB0, 0x20, 0x0B, 0x00, 0x8000);
        rom[0x7FB0 + 0x26] = 0xF3; // cartridge_type -> Cx4
        rom
    }

    /// A build without the chip refuses the cartridge at once, naming it.
    #[cfg(not(feature = "chip-cx4"))]
    #[test]
    fn cx4_cart_is_refused_when_the_chip_is_not_built() {
        let rom = cx4_like_rom(0x20_0000);
        let err = CartridgeBuilder::new(&rom).build().err().expect("refused");
        assert!(
            matches!(err, BuildError::UnsupportedChip { chip: "Cx4" }),
            "{err}"
        );
    }

    /// Resolve a 24-bit S-CPU address through the cartridge's map plan into the
    /// `(chip_id, chip_address)` pair the bus would hand `read_chip`/`write_chip`,
    /// i.e. exercise the real S-CPU window, not a hand-computed base.
    #[cfg(feature = "chip-cx4")]
    fn scpu_chip_addr(cart: &dyn SnesCartridge, addr24: u32) -> (u8, u32) {
        let r = cart.map_plan().resolve(addr24).expect("address maps");
        match cart.map_plan().slots[r.slot_index as usize].target {
            crate::cartridge::MapTarget::ChipMmio { chip_id, base } => {
                (chip_id, base + r.target_offset)
            }
            other => panic!("expected ChipMmio, got {other:?}"),
        }
    }

    #[cfg(feature = "chip-cx4")]
    #[test]
    fn build_cx4_from_detection() {
        // A detected Cx4 cart (MMX2/X3) instantiates the live chip and routes
        // ROM + the $6000-7FFF window through it. The real carts declare no
        // SRAM, so no battery is exposed.
        let rom = cx4_like_rom(0x20_0000);
        let cart = CartridgeBuilder::new(&rom).build().expect("Cx4 builds");
        assert_eq!(cart.map_plan().name, "cx4");
        assert_eq!(cart.coprocessor_label(), Some("Cx4"));

        // ROM is chip-routed (the HG51B169 bus can interpose).
        let r = cart
            .map_plan()
            .resolve(0x00_8000)
            .expect("ROM window mapped");
        let slot = &cart.map_plan().slots[r.slot_index as usize];
        assert!(matches!(
            slot.target,
            crate::cartridge::MapTarget::ChipMmio {
                chip_id: CX4_CHIP_ID,
                ..
            }
        ));

        // The $6000-7FFF window resolves to the chip too.
        let r = cart
            .map_plan()
            .resolve(0x00_7F53)
            .expect("MMIO window mapped");
        let slot = &cart.map_plan().slots[r.slot_index as usize];
        assert!(matches!(
            slot.target,
            crate::cartridge::MapTarget::ChipMmio {
                chip_id: CX4_CHIP_ID,
                ..
            }
        ));

        // No SRAM declared => no `.srm`.
        assert!(cart.sram().is_empty(), "real MMX2/X3 have no battery SRAM");
    }

    #[cfg(feature = "chip-cx4")]
    #[test]
    fn cx4_manifest_sram_is_battery_backed() {
        // RetroLab is a lab for ROMs: a manifest that declares `sram_size > 0`
        // on a Cx4 game gets battery-backed SRAM, even though the real carts
        // have none. The SRAM must round-trip through the S-CPU $70-77 window
        // and surface in `.srm`.
        let rom = cx4_like_rom(0x20_0000);
        let manifest_json = r#"{
            "core": "snes1",
            "version": "1.0",
            "rom": { "sha256": "", "size": 2097152 },
            "mapping": { "preset": "cx4", "sram_size": 8192 }
        }"#;
        let manifest = parse_manifest(manifest_json).unwrap();
        let mut cart = CartridgeBuilder::new(&rom)
            .with_manifest(manifest)
            .build()
            .expect("Cx4 with manifest SRAM builds");
        assert_eq!(cart.coprocessor_label(), Some("Cx4"));
        assert_eq!(cart.sram().len(), 8192, "manifest SRAM is battery-backed");

        // A write through the $70:0000 S-CPU window round-trips and reaches
        // `.srm`; $71:0000 (8 KiB into the linear window, past the 8 KiB buffer)
        // folds back to offset 0 inside the chip.
        let (id, a) = scpu_chip_addr(&*cart, 0x70_0000);
        cart.write_chip(id, a, 0xAB, 0);
        let (id, a) = scpu_chip_addr(&*cart, 0x70_0000);
        assert_eq!(cart.read_chip(id, a, 0), 0xAB);
        assert_eq!(cart.sram()[0], 0xAB, "S-CPU write reaches `.srm`");

        // A loaded `.srm` routes back into the chip's SRAM, readable again.
        let mut srm = vec![0u8; 8192];
        srm[0x40] = 0x5A;
        srm[0x1FFF] = 0x42;
        cart.restore_sram(&srm);
        let (id, a) = scpu_chip_addr(&*cart, 0x70_0040);
        assert_eq!(cart.read_chip(id, a, 0), 0x5A);
        let (id, a) = scpu_chip_addr(&*cart, 0x70_1FFF);
        assert_eq!(cart.read_chip(id, a, 0), 0x42);
    }

    // -- manifest chips[] instantiation via the factory ------------------

    /// Minimal LoROM manifest wrapping one chip block (with optional
    /// trailing fields spliced in by the caller).
    fn lorom_chip_manifest(chip: &str) -> CartridgeManifest {
        let json = format!(
            r#"{{
                "core": "snes1",
                "version": "1.0",
                "rom": {{ "sha256": "", "size": 1048576 }},
                "mapping": {{ "preset": "lorom" }},
                "chips": [ {chip} ]
            }}"#
        );
        parse_manifest(&json).unwrap()
    }

    #[test]
    fn build_manifest_dsp1_chip_is_live_via_factory() {
        // A manifest-declared DSP-1 with no firmware instantiates a live HLE
        // chip through the console factory, not just a ChipMmio slot.
        let rom = lorom_rom(0x10_0000);
        let manifest = lorom_chip_manifest(
            r#"{
                "id": 1,
                "kind": "dsp1",
                "mapping": {
                    "bank_range": ["0x30", "0x3F"],
                    "offset_range": ["0x8000", "0xBFFF"],
                    "target": { "kind": "ChipMmio", "chip_id": 1, "base": 0 },
                    "speed": "Fast"
                }
            }"#,
        );
        let cart = CartridgeBuilder::new(&rom)
            .with_manifest(manifest)
            .build()
            .expect("manifest DSP-1 builds");
        let label = cart.coprocessor_label().expect("DSP-1 is live");
        assert!(label.contains("HLE"), "label = {label}");
    }

    #[test]
    fn build_manifest_unknown_chip_kind_errors() {
        // A chip kind the console factory doesn't know fails the build
        // loudly rather than leaving a silently dead register window. "svp"
        // is a Genesis coprocessor snes1's factory never builds.
        let rom = lorom_rom(0x10_0000);
        let manifest = lorom_chip_manifest(
            r#"{
                "id": 5,
                "kind": "svp",
                "mapping": {
                    "bank_range": ["0x00", "0x3F"],
                    "offset_range": ["0x6000", "0x7FFF"],
                    "target": { "kind": "ChipMmio", "chip_id": 5, "base": 0 },
                    "speed": "Fast"
                }
            }"#,
        );
        let err = expect_err(CartridgeBuilder::new(&rom).with_manifest(manifest).build());
        match err {
            BuildError::UnknownChip { kind } => assert_eq!(kind, "svp"),
            other => panic!("expected UnknownChip, got {:?}", other),
        }
    }

    #[test]
    fn build_manifest_chip_bad_observes_errors() {
        // A malformed `observes` range is caught while assembling the chip,
        // surfaced as a manifest InvalidHex error carrying the field path.
        let rom = lorom_rom(0x10_0000);
        let manifest = lorom_chip_manifest(
            r#"{
                "id": 1,
                "kind": "dsp1",
                "mapping": {
                    "bank_range": ["0x30", "0x3F"],
                    "offset_range": ["0x8000", "0xBFFF"],
                    "target": { "kind": "ChipMmio", "chip_id": 1, "base": 0 },
                    "speed": "Fast"
                },
                "observes": ["not-a-range"]
            }"#,
        );
        let err = expect_err(CartridgeBuilder::new(&rom).with_manifest(manifest).build());
        match err {
            BuildError::Manifest(ManifestError::InvalidHex { field, value }) => {
                assert_eq!(field, "chips[0].observes[0]");
                assert_eq!(value, "not-a-range");
            }
            other => panic!("expected Manifest(InvalidHex), got {:?}", other),
        }
    }

    // -- detected_manifest exporter --------------------------------------

    #[test]
    fn detected_manifest_captures_hirom_sram_fastrom() {
        // DKC shape: HiROM + FastROM (map-mode $31), 2 KiB SRAM.
        let rom = synth_rom(0x40_0000, 0xFFB0, MAP_MODE_HIROM | 0x10, 0x0C, 0x01, 0x8000);
        let manifest = detected_manifest(&rom);

        assert_eq!(manifest.schema.as_deref(), Some("cartridge-manifest-v1"));
        assert_eq!(manifest.core, "snes1");
        assert_eq!(manifest.version, SUPPORTED_MANIFEST_VERSION);
        assert_eq!(manifest.mapping.preset, "hirom");
        assert!(manifest.mapping.fastrom);
        assert_eq!(manifest.mapping.sram_size, 2048);
        assert!(manifest.chips.is_empty());
        // Canonical post-strip digest + size.
        assert_eq!(manifest.rom.sha256, rom_sha256(&rom));
        assert_eq!(manifest.rom.size, 0x40_0000);
        assert_eq!(manifest.rom.region.as_deref(), Some("USA"));
    }

    #[test]
    fn detected_manifest_round_trips_to_same_sram_mask() {
        // Serialize -> parse -> rebuild reproduces the masked SRAM slot.
        let rom = synth_rom(0x40_0000, 0xFFB0, MAP_MODE_HIROM, 0x0C, 0x01, 0x8000);
        let manifest = detected_manifest(&rom);
        let json = serde_json::to_string(&manifest).expect("serializes");
        let parsed = parse_manifest(&json).expect("re-parses");

        let cart = CartridgeBuilder::new(&rom)
            .with_manifest(parsed)
            .build()
            .expect("rebuild from detected manifest");
        let sram = cart
            .map_plan()
            .slots
            .iter()
            .find(|s| matches!(s.target, crate::cartridge::MapTarget::Sram { .. }))
            .expect("SRAM slot present");
        assert_eq!(sram.mask, Some(0x7FF), "2 KiB SRAM mirror mask survives");
    }

    #[test]
    fn detected_manifest_reproduces_lorom_dsp1_plan() {
        // LoROM DSP-1 (map $20, type $03): the detected manifest emits
        // NO chips (the builder re-derives the DSP-1 from the header and
        // front-inserts it), but it records the coprocessor in metadata.
        let mut rom = synth_rom(0x8_0000, 0x7FB0, 0x20, 0x09, 0x00, 0x8000);
        rom[0x7FB0 + 0x26] = 0x03;
        let manifest = detected_manifest(&rom);
        assert_eq!(manifest.mapping.preset, "lorom");
        assert!(manifest.chips.is_empty(), "DSP-1 stays header-derived");
        let meta = manifest.metadata.as_ref().expect("coprocessor metadata");
        assert_eq!(meta["coprocessor"]["kind"], "dsp1");
        assert_eq!(meta["coprocessor"]["layout"], "LoRom1Mb");

        // Re-applying still lands the DSP window ahead of the ROM mirror.
        let cart = CartridgeBuilder::new(&rom)
            .with_manifest(manifest)
            .build()
            .expect("rebuild DSP-1 LoROM");
        let r = cart.map_plan().resolve(0x20_8000).expect("mapped");
        let slot = &cart.map_plan().slots[r.slot_index as usize];
        assert!(
            matches!(slot.target, crate::cartridge::MapTarget::ChipMmio { .. }),
            "DSP-1 window must still win over the ROM mirror after round-trip"
        );
    }

    #[test]
    fn detected_manifest_falls_back_to_lorom_without_header() {
        // A buffer too small to hold any candidate header offset
        // ($7FB0 + 0x50) parses nothing, so the exporter still yields
        // a usable bare LoROM shell with no title / region / chips.
        let rom = vec![0u8; 0x1000];
        let manifest = detected_manifest(&rom);
        assert_eq!(manifest.mapping.preset, "lorom");
        assert_eq!(manifest.mapping.sram_size, 0);
        assert!(!manifest.mapping.fastrom);
        assert!(manifest.rom.title.is_none());
        assert!(manifest.rom.region.is_none());
        assert!(manifest.metadata.is_none());
    }
}
