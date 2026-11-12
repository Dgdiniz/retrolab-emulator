//! Genesis system integration and main emulator struct.

pub mod savestate;
pub mod savestate_impl;

use crate::bus::AddressBus;
use crate::cartridge::composable::CartridgeManifest;
use crate::cpu68k::M68k;
use crate::debugger::{GenesisDebugger, GenesisEvent};
use crate::input_manager::InputManager;
use crate::io::devices::genesis_3button::Genesis3ButtonDevice;
use crate::io::devices::genesis_6button::Genesis6ButtonDevice;
use crate::timing::VideoRegion;
use crate::vdp::dma::MemoryDma;
use crate::z80::Z80;
use retro_core::cpu::Addressable;

use retro_core::*;

/// Self-contained cartridge-only savestate payload: the battery-SRAM image
/// plus every coprocessor's typed state (keyed by `chip_id`). Serialized by
/// [`GenesisEmulator::get_cartridge_save_state`].
type CartridgeSavePayload = (
    Vec<u8>,
    Vec<(u8, crate::cartridge::composable::GenesisCoprocessorState)>,
);

/// Plausible instruction-length offsets `disassemble_window` probes
/// when walking backward from PC. 68K instructions are 2-10 bytes and
/// always even; Z80 instructions are 1-4 bytes (1 byte for plain
/// opcodes through 4 bytes for `DD/FD CB` prefixed instructions).
/// Offsets are tried smallest-first so the shortest matching
/// predecessor wins.
fn probe_offsets(cpu_id: usize) -> &'static [u32] {
    if cpu_id == 1 {
        &[1, 2, 3, 4]
    } else {
        &[2, 4, 6, 8, 10]
    }
}

/// Decode a single instruction at `addr` for the given CPU into a
/// `Code`-kind `DisasmRow`. Shared by the `disassemble_window` driver
/// and `GenesisEmulator::decode_at`.
fn decode_one(emu: &mut GenesisEmulator, cpu_id: usize, addr: u32) -> DisasmRow {
    if cpu_id == 1 {
        // Z80: reuse the trace-format-aware decoder.
        let mut z80_bus = emu.bus.create_z80_bus();
        let (text, size) = crate::z80::disasm::disassemble_at(&mut z80_bus, addr as u16);
        let trace_text = crate::z80::disasm::to_trace_format(&text);
        let (mnemonic, operands) = if let Some(pos) = trace_text.find(' ') {
            (
                trace_text[..pos].to_string(),
                trace_text[pos + 1..].trim().to_string(),
            )
        } else {
            (trace_text.clone(), String::new())
        };
        let mut bytes = Vec::with_capacity(size as usize);
        for i in 0..size {
            bytes.push(z80_bus.read((addr as u16).wrapping_add(i) as u32));
        }
        DisasmRow {
            address: addr,
            kind: DisasmRowKind::Code,
            bytes,
            mnemonic,
            operands,
            comment: None,
        }
    } else {
        let parts = crate::cpu68k::disasm::disassemble_one_parts(&mut emu.bus.peek_view(), addr);
        DisasmRow {
            address: addr,
            kind: DisasmRowKind::Code,
            bytes: parts.bytes,
            mnemonic: parts.mnemonic,
            operands: parts.operands,
            comment: parts.comment,
        }
    }
}

/// The bus accesses one stepped 68000 instruction made, kept while
/// [`GenesisEmulator::set_instruction_access_logging`] is on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstructionAccessLog {
    /// Address of the instruction that made the accesses, 24 bits wide like
    /// the access addresses (the CPU's PC can carry high bits the bus drops).
    pub pc: u32,
    /// `(address, size_in_bytes, is_write, value)` in the order the bus saw
    /// them; `value` is the byte (size 1) or the big-endian word (size 2).
    /// The fetches of the instruction's own opcode and extension words are
    /// among the reads: the bus does not tell them from data reads.
    pub accesses: Vec<(u32, u8, bool, u16)>,
    /// The DMA transfers from 68000 memory the instruction started (its
    /// write to the VDP control port completed a DMA command). The VDP reads
    /// their source itself, so those bytes are not among `accesses`.
    pub dmas: Vec<MemoryDma>,
}

/// Main Genesis emulator struct.
///
/// This implements the `EmulatorCore` trait and ties together all
/// the Genesis hardware components.
pub struct GenesisEmulator {
    system_info: SystemInfo,
    debugger: GenesisDebugger,
    input_manager: InputManager,

    // Hardware components
    cpu: M68k,
    z80: Z80,
    bus: AddressBus,

    // Timing and state
    region: VideoRegion,
    frame_count: u64,
    master_cycles: u64,
    /// Accumulated master clocks for VDP scanline timing during step().
    /// We track in master clocks (CPU cycles x 7) to avoid fractional drift.
    /// 3420 master clocks = 1 scanline (exact), vs 488.57 CPU cycles (inexact).
    step_master_clocks_accumulated: u64,
    /// When true, [`Self::step_68k_internal`] must not advance the VDP beam:
    /// the outer `run_frame` loop owns scanline transitions. Without this, a
    /// line whose 68K budget is 489 cycles (3423 mclks) crosses 3420 inside
    /// `step()` and nudges the beam an extra line on top of the outer `+1`,
    /// drifting EOF scanline by ~1/frame and eventually double-firing VINT.
    frame_runner_owns_beam: bool,
    /// Fractional-cycle carry for the per-scanline 68K budget in the
    /// `run_frame*` tiers. A scanline is `3420 / 7 = 488.571...` 68K cycles, so
    /// each line spends 488 cycles plus a `4/7` remainder. This holds the
    /// numerator (`0..7`) of that remainder and carries across scanlines AND
    /// frames, so the per-frame budget averages the exact `896,040 / 7` (NTSC) rather
    /// than truncating to `488` and losing ~150 cycles/frame.
    scanline_cycle_carry: u32,
    /// 68K instruction overshoot (in 68K cycles) carried past a scanline
    /// boundary. The last instruction of a line usually runs a few cycles past
    /// the budget; those cycles belong to the next line. Persisting this across
    /// frames keeps one continuous cycle count, as the CPU has, instead of
    /// resetting the sub-frame phase every frame.
    frame_cycle_overshoot: u32,
    /// Absolute 68K-cycle target for the next DRAM-refresh bus-steal. On the
    /// Mega Drive the 68000 bus is stolen for DRAM refresh ~every `128` CPU
    /// cycles, stalling the CPU `+2` cycles (the period and the penalty
    /// Genesis Plus GX uses, checked once per
    /// instruction). Whenever `master_cycles` reaches this target we charge the
    /// penalty and advance the target by `128`. Carries across frames.
    bus_refresh_target: u64,
    running: bool,

    // Cached framebuffer (updated from VDP)
    cached_framebuffer: Framebuffer,

    // Last PC where we checked for breakpoint (to avoid double-counting)
    last_bp_checked_pc: Option<u32>,

    /// Bounded ring buffer holding mixed stereo samples ready for the
    /// host. Capacity ~ 4 frames stereo @ 44.1 kHz/60 fps so the host
    /// can tolerate ~67 ms of jitter without the producer dropping
    /// samples or the consumer starving the worklet. The ring MUST
    /// NOT be cleared while playback is live: a mid-stream `clear()`
    /// races the consumer worklet and causes audible ticks. Drop
    /// individual frames by simply not pushing instead.
    audio_ring: retro_core::AudioRingBuffer<f32>,
    /// Scratch buffer used by `EmulatorCore::get_audio_samples()` to
    /// preserve its `&[f32]` return type, drained from the ring on
    /// each call. Kept alive across calls to avoid per-call alloc.
    pending_audio_drain: Vec<f32>,
    audio_sample_rate: u32,
    /// Drift-free cycle->sample mapper. The integer formula
    /// `cumulative = (num * (cycles - ref) + remainder) / den` carries
    /// the fractional remainder forward across frames, so the long-run
    /// sample rate exactly equals `sample_rate_hz * (cpu_cycles / cpu_clock_hz)`
    /// regardless of how many frames have elapsed. Denominator MUST
    /// be the 68K CPU clock (master clock / 7), since `master_cycles`
    /// counts CPU cycles rather than raw master clocks.
    audio_rate_converter: retro_core::IntegerRateConverter,

    // Audio control: enabled by default, can be disabled via API
    audio_enabled: bool,

    // Pre-allocated audio mixing buffers (avoids 3 Vec allocs per frame)
    psg_sample_buf: Vec<f32>,
    ym_sample_buf: Vec<f32>,
    cart_sample_buf: Vec<f32>,

    /// Mixed waveform ring buffer for visualization (mono, last N samples)
    mixed_waveform_buffer: std::collections::VecDeque<f32>,

    /// Previous step's total CPU cycles (including any VDP wait states).
    /// Used to tick the VDP FIFO drain at the start of the next step.
    last_step_cycles: u32,

    /// Z80 maskable interrupt pending flag.
    /// Set true when VBLANK occurs (same signal as 68K level 6 IRQ).
    /// The Z80 checks this via check_interrupts() before each step.
    /// Cleared when the Z80 takes the interrupt or when VBLANK ends; a VDP
    /// status read does not clear it.
    z80_irq_pending: bool,

    /// Accumulated Z80 cycle credit from 68K execution in step mode.
    /// Each 68K cycle generates Z80 credit (68K_cycles * 7 / 15).
    /// Absolute Z80 cycle target for the current scanline in step mode.
    /// Monotonically increases as 68K instructions execute; reset at scanline boundary.
    step_z80_target: u32,

    /// Actual Z80 cycles executed in the current scanline in step mode.
    /// May overshoot the target; reset at scanline boundary.
    step_z80_executed: u32,

    /// Fractional numerator for Z80 cycle target in step mode (/15).
    step_z80_credit_numer: u32,

    /// YM2612 cycles already clocked during step mode Z80 interleaving.
    /// Reset each scanline.
    step_ym_cycles_clocked: u32,

    /// Currently focused CPU for debugging (0 = 68K, 1 = Z80).
    focused_cpu: usize,

    /// Staged cartridge manifest applied on the next [`Self::load_rom`],
    /// overriding heuristic detection. Set from the host via
    /// [`Self::set_cartridge_manifest_override`]; `None` means "use pure
    /// detection". Editing it does not disturb an already-loaded cartridge.
    cartridge_manifest_override: Option<CartridgeManifest>,

    /// The raw bytes of the most recently loaded ROM, kept so
    /// [`Self::detected_manifest`] can re-run the heuristic detector for
    /// the *currently loaded* game without the host re-supplying them.
    last_rom: Vec<u8>,

    /// Whether the stepping paths keep each 68000 instruction's bus
    /// accesses in `instruction_access_log`. Debugger state, never part of
    /// a savestate.
    instruction_access_logging: bool,

    /// The bus accesses of the last instruction stepped while
    /// `instruction_access_logging` is on.
    instruction_access_log: InstructionAccessLog,
}

impl GenesisEmulator {
    /// Create a new Genesis emulator instance.
    pub fn new() -> Self {
        let system_info = SystemInfo {
            name: "Sega Genesis".to_string(),
            short_name: "genesis".to_string(),
            manufacturer: "Sega".to_string(),
            year: 1988,
            cpus: vec![
                CpuInfo {
                    id: 0,
                    name: "Motorola 68000".to_string(),
                    architecture: CpuArchitecture::M68000,
                    clock_speed: 7_670_454, // ~7.67 MHz
                    location: CpuLocation::Motherboard,
                    address_width: 24,
                    registers: vec![
                        // Data registers
                        RegisterInfo {
                            name: "D0".to_string(),
                            size_bits: 32,
                            display_format: DisplayFormat::Hexadecimal,
                            category: RegisterCategory::General,
                        },
                        RegisterInfo {
                            name: "D1".to_string(),
                            size_bits: 32,
                            display_format: DisplayFormat::Hexadecimal,
                            category: RegisterCategory::General,
                        },
                        RegisterInfo {
                            name: "D2".to_string(),
                            size_bits: 32,
                            display_format: DisplayFormat::Hexadecimal,
                            category: RegisterCategory::General,
                        },
                        RegisterInfo {
                            name: "D3".to_string(),
                            size_bits: 32,
                            display_format: DisplayFormat::Hexadecimal,
                            category: RegisterCategory::General,
                        },
                        RegisterInfo {
                            name: "D4".to_string(),
                            size_bits: 32,
                            display_format: DisplayFormat::Hexadecimal,
                            category: RegisterCategory::General,
                        },
                        RegisterInfo {
                            name: "D5".to_string(),
                            size_bits: 32,
                            display_format: DisplayFormat::Hexadecimal,
                            category: RegisterCategory::General,
                        },
                        RegisterInfo {
                            name: "D6".to_string(),
                            size_bits: 32,
                            display_format: DisplayFormat::Hexadecimal,
                            category: RegisterCategory::General,
                        },
                        RegisterInfo {
                            name: "D7".to_string(),
                            size_bits: 32,
                            display_format: DisplayFormat::Hexadecimal,
                            category: RegisterCategory::General,
                        },
                        // Address registers
                        RegisterInfo {
                            name: "A0".to_string(),
                            size_bits: 32,
                            display_format: DisplayFormat::Address,
                            category: RegisterCategory::Address,
                        },
                        RegisterInfo {
                            name: "A1".to_string(),
                            size_bits: 32,
                            display_format: DisplayFormat::Address,
                            category: RegisterCategory::Address,
                        },
                        RegisterInfo {
                            name: "A2".to_string(),
                            size_bits: 32,
                            display_format: DisplayFormat::Address,
                            category: RegisterCategory::Address,
                        },
                        RegisterInfo {
                            name: "A3".to_string(),
                            size_bits: 32,
                            display_format: DisplayFormat::Address,
                            category: RegisterCategory::Address,
                        },
                        RegisterInfo {
                            name: "A4".to_string(),
                            size_bits: 32,
                            display_format: DisplayFormat::Address,
                            category: RegisterCategory::Address,
                        },
                        RegisterInfo {
                            name: "A5".to_string(),
                            size_bits: 32,
                            display_format: DisplayFormat::Address,
                            category: RegisterCategory::Address,
                        },
                        RegisterInfo {
                            name: "A6".to_string(),
                            size_bits: 32,
                            display_format: DisplayFormat::Address,
                            category: RegisterCategory::Address,
                        },
                        RegisterInfo {
                            name: "A7".to_string(),
                            size_bits: 32,
                            display_format: DisplayFormat::Address,
                            category: RegisterCategory::Address,
                        },
                        // Special registers
                        RegisterInfo {
                            name: "PC".to_string(),
                            size_bits: 32,
                            display_format: DisplayFormat::Address,
                            category: RegisterCategory::Special,
                        },
                        RegisterInfo {
                            name: "SR".to_string(),
                            size_bits: 16,
                            display_format: DisplayFormat::Binary,
                            category: RegisterCategory::Flags,
                        },
                    ],
                },
                CpuInfo {
                    id: 1,
                    name: "Zilog Z80".to_string(),
                    architecture: CpuArchitecture::Z80,
                    clock_speed: 3_579_545, // ~3.58 MHz
                    location: CpuLocation::Motherboard,
                    address_width: 16,
                    registers: vec![
                        // Main register set
                        RegisterInfo {
                            name: "A".to_string(),
                            size_bits: 8,
                            display_format: DisplayFormat::Hexadecimal,
                            category: RegisterCategory::General,
                        },
                        RegisterInfo {
                            name: "F".to_string(),
                            size_bits: 8,
                            display_format: DisplayFormat::Binary,
                            category: RegisterCategory::Flags,
                        },
                        RegisterInfo {
                            name: "B".to_string(),
                            size_bits: 8,
                            display_format: DisplayFormat::Hexadecimal,
                            category: RegisterCategory::General,
                        },
                        RegisterInfo {
                            name: "C".to_string(),
                            size_bits: 8,
                            display_format: DisplayFormat::Hexadecimal,
                            category: RegisterCategory::General,
                        },
                        RegisterInfo {
                            name: "D".to_string(),
                            size_bits: 8,
                            display_format: DisplayFormat::Hexadecimal,
                            category: RegisterCategory::General,
                        },
                        RegisterInfo {
                            name: "E".to_string(),
                            size_bits: 8,
                            display_format: DisplayFormat::Hexadecimal,
                            category: RegisterCategory::General,
                        },
                        RegisterInfo {
                            name: "H".to_string(),
                            size_bits: 8,
                            display_format: DisplayFormat::Hexadecimal,
                            category: RegisterCategory::General,
                        },
                        RegisterInfo {
                            name: "L".to_string(),
                            size_bits: 8,
                            display_format: DisplayFormat::Hexadecimal,
                            category: RegisterCategory::General,
                        },
                        // Special registers
                        RegisterInfo {
                            name: "PC".to_string(),
                            size_bits: 16,
                            display_format: DisplayFormat::Address,
                            category: RegisterCategory::Special,
                        },
                        RegisterInfo {
                            name: "SP".to_string(),
                            size_bits: 16,
                            display_format: DisplayFormat::Address,
                            category: RegisterCategory::Special,
                        },
                        RegisterInfo {
                            name: "IX".to_string(),
                            size_bits: 16,
                            display_format: DisplayFormat::Address,
                            category: RegisterCategory::Address, // Index register (address-like)
                        },
                        RegisterInfo {
                            name: "IY".to_string(),
                            size_bits: 16,
                            display_format: DisplayFormat::Address,
                            category: RegisterCategory::Address, // Index register (address-like)
                        },
                    ],
                },
            ],
            display: DisplayInfo {
                width: 320,
                height: 224,
                aspect_ratio: 4.0 / 3.0,
                refresh_rate: VideoRegion::default().frame_rate(),
                pixel_format: PixelFormat::RGBA8888,
            },
            audio: AudioInfo {
                sample_rate: 44100,
                channels: 2,
                bits_per_sample: 16,
            },
            input: InputInfo {
                max_players: 2,
                button_layout: vec![
                    ButtonInfo {
                        name: "Up".to_string(),
                        id: 0,
                    },
                    ButtonInfo {
                        name: "Down".to_string(),
                        id: 1,
                    },
                    ButtonInfo {
                        name: "Left".to_string(),
                        id: 2,
                    },
                    ButtonInfo {
                        name: "Right".to_string(),
                        id: 3,
                    },
                    ButtonInfo {
                        name: "A".to_string(),
                        id: 4,
                    },
                    ButtonInfo {
                        name: "B".to_string(),
                        id: 5,
                    },
                    ButtonInfo {
                        name: "C".to_string(),
                        id: 6,
                    },
                    ButtonInfo {
                        name: "Start".to_string(),
                        id: 7,
                    },
                    ButtonInfo {
                        name: "X".to_string(),
                        id: 8,
                    },
                    ButtonInfo {
                        name: "Y".to_string(),
                        id: 9,
                    },
                    ButtonInfo {
                        name: "Z".to_string(),
                        id: 10,
                    },
                    ButtonInfo {
                        name: "Mode".to_string(),
                        id: 11,
                    },
                ],
            },
        };

        // Create bus and connect 3-button controllers to ports 0 and 1
        let mut bus = AddressBus::new();
        bus.io_mut()
            .port_mut(0)
            .unwrap()
            .connect_device(Box::new(Genesis3ButtonDevice::new()));
        bus.io_mut()
            .port_mut(1)
            .unwrap()
            .connect_device(Box::new(Genesis3ButtonDevice::new()));

        Self {
            debugger: GenesisDebugger::new_with_system_info(system_info.clone()),
            input_manager: InputManager::new(),
            system_info,
            cpu: M68k::new(),
            z80: Z80::new(),
            bus,
            region: VideoRegion::default(),
            frame_count: 0,
            // MAME shows totalcycles=0x22 (34) after hard reset, before first instruction
            // This represents the reset sequence cycles. We match this for trace compatibility.
            master_cycles: 34,
            // Convert initial CPU cycles to master clocks: 34 * 7 = 238 master clocks
            // This ensures scanline tracking starts at the correct position within the scanline
            step_master_clocks_accumulated: 34 * 7,
            frame_runner_owns_beam: false,
            scanline_cycle_carry: 0,
            frame_cycle_overshoot: 0,
            // First refresh bus-steal lands at the 128-cycle boundary after the
            // 34-cycle reset sequence.
            bus_refresh_target: 128,
            running: false,
            cached_framebuffer: Framebuffer::new(320, 224),
            last_bp_checked_pc: None,

            // ~4 frames stereo @ 44.1 kHz/60 fps; bounded, overflow
            // drops oldest samples (host already drained them in the
            // previous frame in the typical case).
            audio_ring: retro_core::AudioRingBuffer::with_capacity(5880),
            pending_audio_drain: Vec::with_capacity(2048),
            audio_sample_rate: 44100,
            // The converter's denominator is the **68K CPU clock**
            // (master_clock / 7 ~ 7.67 MHz NTSC), because
            // `self.master_cycles` is a 68K CPU-cycle counter, NOT
            // master clocks. Using master_clock here would produce
            // ~1/7 of the expected sample count per frame and the
            // converter would silently underrun the host.
            audio_rate_converter: retro_core::IntegerRateConverter::new(
                44_100,
                VideoRegion::default().cpu_clock(),
            ),

            // Audio enabled by default
            audio_enabled: true,

            // Pre-allocated audio mixing buffers (~735 samples/frame at 44.1kHz/60fps)
            psg_sample_buf: vec![0.0; 1024],
            ym_sample_buf: vec![0.0; 2048],
            cart_sample_buf: vec![0.0; 2048],
            mixed_waveform_buffer: std::collections::VecDeque::with_capacity(2048),

            // Previous step's total cycles for FIFO drain timing
            last_step_cycles: 0,
            z80_irq_pending: false,
            step_z80_target: 0,
            step_z80_executed: 0,
            step_z80_credit_numer: 0,
            step_ym_cycles_clocked: 0,
            focused_cpu: 0,
            cartridge_manifest_override: None,
            last_rom: Vec::new(),
            instruction_access_logging: false,
            instruction_access_log: InstructionAccessLog::default(),
        }
    }

    /// Keep (`true`) or stop keeping the bus accesses each 68000
    /// instruction makes, for [`Self::take_instruction_access_log`]. The
    /// paths that run one instruction at a time keep them
    /// ([`EmulatorCore::step`], which the full debug frame loop runs too, and
    /// the light debug loop); the fast frame loop never does. Only the
    /// instruction's own accesses are kept, not those of an interrupt entered
    /// before it nor the Z80's; a DMA from 68000 memory the instruction
    /// starts is kept beside them.
    pub fn set_instruction_access_logging(&mut self, enabled: bool) {
        self.instruction_access_logging = enabled;
        self.instruction_access_log = InstructionAccessLog::default();
    }

    /// The bus accesses of the last 68000 instruction stepped while
    /// [`Self::set_instruction_access_logging`] is on, leaving the log
    /// empty, so a step that ran no instruction (the CPU stopped, waiting
    /// for an interrupt) returns nothing.
    pub fn take_instruction_access_log(&mut self) -> InstructionAccessLog {
        std::mem::take(&mut self.instruction_access_log)
    }

    /// Stage a cartridge manifest applied on the next [`Self::load_rom`],
    /// overriding heuristic detection. `None` reverts to pure detection.
    /// Does not disturb an already-installed cartridge: reload the ROM to apply it.
    pub fn set_cartridge_manifest_override(&mut self, manifest: Option<CartridgeManifest>) {
        self.cartridge_manifest_override = manifest;
    }

    /// The staged manifest override, if any.
    pub fn cartridge_manifest_override(&self) -> Option<&CartridgeManifest> {
        self.cartridge_manifest_override.as_ref()
    }

    /// The editable `cartridge-manifest-v1` the heuristic detector resolves
    /// for `rom`, with no override applied. Side-effect-free.
    pub fn detected_manifest(&self, rom: &[u8]) -> CartridgeManifest {
        crate::cartridge::composable::detected_manifest(rom)
    }

    /// The raw bytes of the currently loaded ROM (empty when none loaded),
    /// so the host can re-detect the manifest for the live game.
    pub fn last_rom(&self) -> &[u8] {
        &self.last_rom
    }

    /// The installed cartridge's map-plan regions, for the cartridge
    /// debugger's memory-map view. Empty when no cartridge is loaded.
    pub fn cartridge_map_regions(&self) -> Vec<crate::cartridge::composable::MapRegionSummary> {
        self.bus
            .cartridge()
            .map(|cart| crate::cartridge::composable::describe_map_plan(cart.map_plan()))
            .unwrap_or_default()
    }

    /// Get the current video region (NTSC/PAL).
    pub fn region(&self) -> VideoRegion {
        self.region
    }

    /// Get VDP reference for debugging.
    pub fn vdp(&self) -> &crate::vdp::Vdp {
        self.bus.vdp()
    }

    /// Get mutable VDP reference for debugging/testing.
    pub fn vdp_mut(&mut self) -> &mut crate::vdp::Vdp {
        self.bus.vdp_mut()
    }

    /// Live video debugger over the VDP: per-layer toggles plus
    /// tile/palette/sprite/register inspection through the generic
    /// `retro_core::video::VideoDebugger` interface. Mirrors
    /// `SnesEmulator::video_debugger()` so the host drives both consoles
    /// through the same trait.
    pub fn video_debugger(&mut self) -> &mut dyn retro_core::video::VideoDebugger {
        self.bus.vdp_mut()
    }

    /// Live audio debugger over the primary FM chip (YM2612): channel,
    /// operator, and register inspection through the generic
    /// `retro_core::audio::AudioDebugger` interface. Mirrors
    /// `SnesEmulator::audio_debugger()`. The PSG is a second audio chip
    /// reached via [`Self::psg`]; the generic single-chip trait exposes the
    /// FM synth, which is what the host's primary audio panels drive.
    pub fn audio_debugger(&mut self) -> &mut dyn retro_core::audio::AudioDebugger {
        self.bus.ym2612_mut()
    }

    /// A [`CartridgeDebugger`](retro_core::cartridge::CartridgeDebugger) over
    /// the installed cartridge: memory-map / ROM / SRAM regions, the mapper
    /// state, the per-chip coprocessor snapshots, and ROM/SRAM breakpoints
    /// (which append to the emulator's shared breakpoint list). Mirrors
    /// `SnesEmulator::cartridge_debugger()`. With no cartridge loaded it
    /// binds an empty plan so the surface stays non-`Option`.
    pub fn cartridge_debugger(
        &mut self,
    ) -> crate::cartridge::composable::GenesisCartridgeDebugger<'_> {
        let (plan, rom_size, sram_size, chip_states) = match self.bus.cartridge() {
            Some(cart) => (
                cart.map_plan().clone(),
                cart.info().rom_size as u32,
                cart.sram().len() as u32,
                cart.coprocessors_debug_state(),
            ),
            None => (
                crate::cartridge::composable::MapPlan::default(),
                0,
                0,
                Vec::new(),
            ),
        };
        crate::cartridge::composable::GenesisCartridgeDebugger::new(
            plan,
            rom_size,
            sram_size,
            chip_states,
            self.debugger.breakpoints_mut(),
        )
    }

    /// Live debug snapshot of the primary (lowest-id) coprocessor (the SVP's
    /// SSP1601 registers on Virtua Racing), or `None` on a plain cartridge.
    pub fn coprocessor_debug_state(&self) -> Option<retro_core::cartridge::ExpansionChipState> {
        self.bus
            .cartridge()
            .and_then(|cart| cart.coprocessor_debug_state())
    }

    /// Live debug snapshots of every coprocessor, keyed by `chip_id`. Empty
    /// on a plain cartridge.
    pub fn coprocessors_debug_state(&self) -> Vec<(u8, retro_core::cartridge::ExpansionChipState)> {
        self.bus
            .cartridge()
            .map(|cart| cart.coprocessors_debug_state())
            .unwrap_or_default()
    }

    /// Human-readable label of every coprocessor, keyed by `chip_id` (e.g.
    /// `[(0, "SVP")]`). Empty on a plain cartridge.
    pub fn coprocessor_labels(&self) -> Vec<(u8, String)> {
        self.bus
            .cartridge()
            .map(|cart| cart.coprocessor_labels())
            .unwrap_or_default()
    }

    /// MMIO access count of the primary coprocessor (proves the game is
    /// driving it). Zero when there is no coprocessor.
    pub fn coprocessor_accesses(&self) -> u64 {
        self.bus
            .cartridge()
            .map(|cart| cart.coprocessor_accesses())
            .unwrap_or(0)
    }

    /// Re-render the currently held frame from live VDP state, applying the
    /// debugger's per-layer mask, and refresh the host-visible cached
    /// framebuffer, all without advancing emulation by a single cycle.
    ///
    /// The host reads frames through [`Self::get_framebuffer`], which returns
    /// the cached copy the run loops refresh once per frame via
    /// `update_cached_framebuffer`. A paused core never re-composites, so a
    /// debugger layer toggle (or a VRAM/CRAM edit) would stay invisible until
    /// the game resumes. Driving the VDP re-render alone is not enough: the
    /// new pixels land in the VDP's own framebuffer, not the cache the host
    /// reads, so this re-syncs the cache too. Mirrors the per-frame cache
    /// refresh the `run_frame*` paths already perform.
    pub fn repaint_held_frame(&mut self) {
        self.bus.vdp_mut().rerender_current_frame();
        self.update_cached_framebuffer();
    }

    /// Set controller port button state directly (for CLI input injection).
    /// `state` is a bitmask: bit 0=UP, 1=DOWN, 2=LEFT, 3=RIGHT, 4=B, 5=C, 6=A, 7=START.
    pub fn set_port_button_state(&mut self, port: usize, state: u32) {
        if let Some(p) = self.bus.io_mut().port_mut(port) {
            p.set_device_button_state(state);
        }
    }

    /// True when the cartridge header's I/O-support field ($190-$19F)
    /// contains `'6'` (6-button pad).
    pub fn rom_advertises_six_button(rom: &[u8]) -> bool {
        if rom.len() < 0x1A0 {
            return false;
        }
        rom[0x190..0x1A0].contains(&b'6')
    }

    /// Install 3- or 6-button pads on ports 0/1 from the ROM header.
    pub fn connect_controllers_for_rom(&mut self, rom: &[u8]) {
        self.connect_controllers(Self::rom_advertises_six_button(rom));
    }

    /// Install 3- or 6-button pads on both DE-9 ports.
    pub fn connect_controllers(&mut self, six_button: bool) {
        for port in 0..2 {
            if let Some(p) = self.bus.io_mut().port_mut(port) {
                if six_button {
                    p.connect_device(Box::new(Genesis6ButtonDevice::new()));
                } else {
                    p.connect_device(Box::new(Genesis3ButtonDevice::new()));
                }
            }
        }
    }

    /// Get PSG reference.
    pub fn psg(&self) -> &crate::psg::Psg {
        self.bus.psg()
    }

    /// Get mutable PSG reference.
    pub fn psg_mut(&mut self) -> &mut crate::psg::Psg {
        self.bus.psg_mut()
    }

    // === YM2612 Audio Chip Access ===

    /// Get immutable YM2612 reference.
    pub fn ym2612(&self) -> &crate::ym2612::Ym2612 {
        self.bus.ym2612()
    }

    /// Get mutable YM2612 reference.
    pub fn ym2612_mut(&mut self) -> &mut crate::ym2612::Ym2612 {
        self.bus.ym2612_mut()
    }

    /// Get YM2612 channel state for debugging.
    pub fn get_ym2612_channel_state(
        &self,
        channel: u32,
    ) -> Option<retro_core::audio::ChannelState> {
        use retro_core::audio::AudioDebugger;
        self.ym2612().get_channel_state(channel)
    }

    /// Mute/unmute YM2612 channel.
    pub fn mute_ym2612_channel(&mut self, channel: u32, muted: bool) {
        use retro_core::audio::AudioDebugger;
        self.ym2612_mut().mute_channel(channel, muted);
    }

    /// Get YM2612 LFO state.
    pub fn get_ym2612_lfo_state(&self) -> serde_json::Value {
        let lfo = self.ym2612().lfo();
        serde_json::json!({
            "enabled": lfo.is_enabled(),
            "frequency": lfo.get_frequency(),
        })
    }

    /// Get YM2612 DAC state.
    pub fn get_ym2612_dac_state(&self) -> serde_json::Value {
        let ym = self.ym2612();
        serde_json::json!({
            "enabled": ym.is_dac_enabled(),
        })
    }

    /// Get YM2612 Timer A state.
    pub fn get_ym2612_timer_a_state(&self) -> serde_json::Value {
        let timers = self.ym2612().timers();
        serde_json::json!({
            "running": timers.is_timer_a_running(),
            "period": timers.get_timer_a_period(),
            "overflowed": (timers.get_status() & 0x01) != 0,
        })
    }

    /// Get YM2612 Timer B state.
    pub fn get_ym2612_timer_b_state(&self) -> serde_json::Value {
        let timers = self.ym2612().timers();
        serde_json::json!({
            "running": timers.is_timer_b_running(),
            "period": timers.get_timer_b_period(),
            "overflowed": (timers.get_status() & 0x02) != 0,
        })
    }

    /// Drain every available sample from the audio ring into a
    /// freshly-allocated `Vec<f32>`.
    ///
    /// Convenience API for callers (CLI, integration tests) that need
    /// an owned `Vec`. Hosts on the hot path should prefer
    /// [`Self::drain_audio_into`] (zero-alloc pull) and
    /// [`Self::audio_available`] (peek the queued count).
    pub fn get_audio_buffer(&mut self) -> Vec<f32> {
        let n = self.audio_ring.available();
        let mut out = vec![0.0; n];
        self.audio_ring.drain_into(&mut out);
        out
    }

    /// Drop every queued sample without delivering it. Used by reset
    /// paths and `set_audio_enabled(false)`.
    ///
    /// **Do not call from mid-stream code paths.** The ring is shared
    /// with the host's audio worklet (via repeated drains); clearing
    /// it while playback is live races the consumer and produces
    /// audible ticks. Hosts that want to skip ahead should simply
    /// stop pulling: overflow drops oldest samples automatically.
    pub fn clear_audio_buffer(&mut self) {
        self.audio_ring.clear();
    }

    /// Drop audio that has been generated but not yet handed to the host.
    ///
    /// Deliberately empty on this core, and that is the whole point: a
    /// headless frame here never queues anything. `generate_and_mix_audio`
    /// advances the converter and both chips, serialized state the desync
    /// hash covers, and then returns *before* the mix and the ring push, so
    /// a rollback re-simulation leaves nothing pending to discard. Clearing
    /// the host ring instead would throw away audio that was presented and
    /// is still on its way to the speaker.
    ///
    /// Exists so the netplay runner has one call it can make after a re-sim
    /// without asking which core it is talking to.
    pub fn discard_pending_audio(&mut self) {}

    /// Drain up to `out.len()` queued samples into `out`. Returns the
    /// number actually written; if fewer than `out.len()` are
    /// available the tail of `out` is left untouched (caller must
    /// `truncate` if they want the exact count).
    ///
    /// Zero-alloc pull path. Wired through the WASM bridge as
    /// `getAudioSamplesN(n)`.
    pub fn drain_audio_into(&mut self, out: &mut [f32]) -> usize {
        self.audio_ring.drain_into(out)
    }

    /// Number of stereo-interleaved samples currently queued. Used
    /// by the host to right-size pulls, see `getAudioAvailable()` in
    /// the WASM bridge.
    pub fn audio_available(&self) -> usize {
        self.audio_ring.available()
    }

    /// Cumulative count of "silence fillers" the ring inserted on
    /// drain because the host asked for more than was available.
    /// A non-zero value means the producer is behind, the symptom
    /// the user hears as audio dropouts. Exposed for telemetry /
    /// diagnostics.
    pub fn audio_ring_underruns(&self) -> u64 {
        self.audio_ring.underrun_count()
    }

    /// Cumulative count of samples the ring dropped from the front
    /// because incoming data wouldn't fit. > 0 means the host is
    /// behind, usually a stalled animation frame loop. Useful for
    /// the same telemetry overlay as `audio_ring_underruns()`.
    pub fn audio_ring_overflows(&self) -> u64 {
        self.audio_ring.overflow_count()
    }

    /// Get the mixed waveform ring buffer for visualization.
    /// Returns the last N mono samples of the mixed PSG+YM2612 output.
    pub fn get_mixed_waveform(&self, samples: u32) -> Vec<f32> {
        let count = (samples as usize).min(self.mixed_waveform_buffer.len());
        if count == 0 {
            return vec![0.0; samples as usize];
        }
        let skip = self.mixed_waveform_buffer.len() - count;
        let mut result: Vec<f32> = self
            .mixed_waveform_buffer
            .iter()
            .skip(skip)
            .copied()
            .collect();
        // Pad front with zeros if not enough samples yet
        if result.len() < samples as usize {
            let mut padded = vec![0.0; samples as usize - result.len()];
            padded.append(&mut result);
            padded
        } else {
            result
        }
    }

    /// Set the video region.
    ///
    /// Recreates the audio rate converter with the new region's 68K
    /// CPU clock so the cycles->samples mapping stays accurate across
    /// an NTSC<->PAL switch, and re-anchors it at the current
    /// `master_cycles` so no audible discontinuity is introduced.
    ///
    /// Also pushes the region into the VDP so the system-level scanline
    /// budget (`region.scanlines()`) and the VDP's own timing (line
    /// count, VBLANK window) stay coherent; otherwise a PAL system
    /// would run 313-line frames against an NTSC VDP. For netplay both
    /// peers agree on region in the session handshake and call this
    /// right after `load_rom`, before running any frame.
    pub fn set_region(&mut self, region: VideoRegion) {
        self.region = region;
        // The published refresh rate follows the region: it is what the
        // host paces frames against, so a PAL machine must advertise 49.7 Hz.
        self.system_info.display.refresh_rate = region.frame_rate();
        let mut conv = retro_core::IntegerRateConverter::new(
            self.audio_sample_rate as u64,
            region.cpu_clock(),
        );
        conv.reset(self.master_cycles);
        self.audio_rate_converter = conv;

        // Keep the VDP's region in lockstep with the system region.
        let vdp_region = match region {
            VideoRegion::Ntsc => crate::vdp::timing::VideoRegion::Ntsc,
            VideoRegion::Pal => crate::vdp::timing::VideoRegion::Pal,
        };
        self.bus.vdp_mut().set_region(vdp_region);
    }

    /// Set the video region from a case-insensitive `"ntsc"`/`"pal"`
    /// string. The netplay session handshake carries region as a
    /// string, so this keeps the enum out of the WASM/JS boundary.
    pub fn set_region_str(&mut self, name: &str) -> Result<(), String> {
        let region = match name.trim().to_ascii_lowercase().as_str() {
            "ntsc" => VideoRegion::Ntsc,
            "pal" => VideoRegion::Pal,
            other => {
                return Err(format!(
                    "unknown region '{other}' (expected 'ntsc' or 'pal')"
                ))
            }
        };
        self.set_region(region);
        Ok(())
    }

    /// Current region as the `"ntsc"`/`"pal"` string used by the
    /// netplay handshake and the `getRegion` WASM export.
    pub fn region_str(&self) -> &'static str {
        match self.region {
            VideoRegion::Ntsc => "ntsc",
            VideoRegion::Pal => "pal",
        }
    }

    /// Get mutable reference to the debugger.
    pub fn debugger(&mut self) -> &mut GenesisDebugger {
        &mut self.debugger
    }

    /// Get immutable reference to the debugger (for checking state without borrowing mutably).
    pub fn debugger_ref(&self) -> &GenesisDebugger {
        &self.debugger
    }

    /// Set the debugger's execution mode. A pause also writes the CPU states
    /// the debugger reports: a game running with no breakpoint and no trace
    /// runs the fast loop, which never writes them, so without it a pause
    /// would report the state of the last reset or step.
    pub fn set_execution_mode(&mut self, mode: ExecutionMode) {
        self.debugger.set_execution_mode(mode);
        if mode == ExecutionMode::Paused {
            self.update_debugger_state();
        }
    }

    // ===== Tracing Methods =====

    /// Enable or disable execution tracing.
    pub fn set_tracing_enabled(&mut self, enabled: bool) {
        self.debugger.set_tracing_enabled(enabled);
    }

    /// Check if tracing is enabled.
    pub fn is_tracing_enabled(&self) -> bool {
        self.debugger.is_68k_tracing_enabled()
    }

    /// Check if trace buffer needs to be flushed.
    pub fn needs_trace_flush(&self) -> bool {
        self.debugger.needs_trace_flush()
    }

    /// Get trace data ready for flushing.
    pub fn get_trace_flush_data(&self) -> &[u8] {
        self.debugger.get_trace_flush_data()
    }

    /// Clear flushed trace data.
    pub fn clear_trace_flushed(&mut self) {
        self.debugger.clear_trace_flushed();
    }

    /// Get tracing statistics.
    pub fn get_trace_stats(&self) -> retro_core::TraceStats {
        self.debugger.get_trace_stats()
    }

    // ===== Z80 Tracing =====

    /// Enable or disable Z80 execution tracing.
    pub fn set_z80_tracing_enabled(&mut self, enabled: bool) {
        self.debugger.set_z80_tracing_enabled(enabled);
    }

    /// Check if Z80 tracing is enabled.
    pub fn is_z80_tracing_enabled(&self) -> bool {
        self.debugger.is_z80_tracing_enabled()
    }

    /// Check if Z80 trace buffer needs to be flushed.
    pub fn needs_z80_trace_flush(&self) -> bool {
        self.debugger.needs_z80_trace_flush()
    }

    /// Get Z80 trace data ready for flushing.
    pub fn get_z80_trace_flush_data(&self) -> &[u8] {
        self.debugger.get_z80_trace_flush_data()
    }

    /// Clear flushed Z80 trace data.
    pub fn clear_z80_trace_flushed(&mut self) {
        self.debugger.clear_z80_trace_flushed();
    }

    /// Get the number of frames executed.
    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    /// Cold (power-on) reset: rebuild the emulator to the state [`Self::new`]
    /// and `load_rom` give, so work RAM and Z80 RAM start zeroed.
    ///
    /// The plain `reset()` emulates the Genesis *reset button*, which
    /// preserves RAM on real hardware; this emulates a *power cycle*, which does
    /// not. Netplay needs it: two peers must derive a byte-identical frame-0
    /// state hash, but a host that had been running the game (dirty RAM) and a
    /// freshly-booted guest reset to frame 0 with *different* RAM contents, since
    /// `reset()` leaves RAM untouched. Zeroing it here gives both peers the same
    /// deterministic power-on baseline. The cartridge ROM is preserved, but
    /// battery-backed **SRAM is deliberately blanked** to a fresh image: the
    /// cartridge is rebuilt from the ROM bytes, so both peers start byte-identical
    /// regardless of either one's save file (a save-resume start arrives through
    /// the seeded savestate transfer, not through this cold boot). That makes this
    /// UNSAFE behind a user-facing Reset button: use [`Self::hard_reset`], which
    /// power-cycles the same volatile hardware while keeping the save.
    pub fn power_on_reset(&mut self) {
        // A soft reset (the console's reset *button*, `EmulatorCore::reset`)
        // re-boots the 68000 (PC/SP/SR from the vector table; D0-D7/A0-A6,
        // undefined on real hardware after reset, are zeroed) and clears
        // VRAM/CRAM/VSRAM and the audio chips, but leaves work RAM, Z80 RAM, the
        // USP, the VDP registers other than R0/R1/R12/R15, the I/O ports and the
        // Z80 bank register holding whatever the running game left there. A
        // real *power* cycle clears all of it. Netplay needs the latter: two peers
        // with different pre-session histories (a host mid-game vs a just-joined
        // guest) MUST converge on a byte-identical frame-0 state for the
        // start-commit baseline to match, and they diverge on exactly those
        // subsystems a soft reset preserves.
        //
        // Rebuild the whole machine to the state a freshly-constructed emulator +
        // `load_rom` produces (byte-for-byte what a just-joined guest holds),
        // preserving only what is invariant across peers: the ROM image, the
        // cartridge manifest override, and the video region (all three pinned
        // identical by the netplay handshake). Because both peers run this exact
        // logic over identical preserved inputs, the result is identical by
        // construction. Battery-backed SRAM is intentionally reset to a fresh
        // (empty) image: a game with a save resumes through the seeded savestate
        // transfer, NOT this cold boot, so both peers still start byte-identical.
        let rom = std::mem::take(&mut self.last_rom);
        let manifest = self.cartridge_manifest_override.take();
        let region = self.region;

        *self = GenesisEmulator::new();
        // Must precede `load_rom`, which reads it to build the cartridge.
        self.cartridge_manifest_override = manifest;
        if !rom.is_empty() {
            // These bytes already loaded once, so a re-load cannot fail; swallow
            // the Result rather than propagate: `power_on_reset` is infallible.
            let _ = <Self as EmulatorCore>::load_rom(self, &rom);
        }
        // Re-apply the region *after* load_rom (mirroring the normal load ->
        // set-region order) through the setter, which rebuilds the audio-rate
        // converter for the region's clock and syncs the VDP region; `new()`
        // starts at the NTSC default.
        self.set_region(region);
    }

    /// Rebuild every volatile subsystem to its power-on state, keeping the
    /// cartridge plugged in.
    ///
    /// The address bus owns work RAM, Z80 RAM, the VDP (registers **and**
    /// VRAM/CRAM/VSRAM), the PSG, the YM2612, the I/O ports and the Z80
    /// bus/reset/bank latches, among them everything [`EmulatorCore::reset`]
    /// deliberately leaves alone because a real Genesis *reset button* does
    /// not clear it (work RAM, Z80 RAM, most VDP registers, the I/O ports,
    /// the Z80 bank register). Replacing the bus wholesale is therefore the
    /// console's power switch. The cartridge is unplugged first and plugged
    /// straight back in so its ROM and battery-backed SRAM survive, exactly
    /// as they do across a real power cycle.
    ///
    /// Callers must run [`EmulatorCore::reset`] afterwards to boot the CPUs
    /// off the (now freshly mapped) vector table.
    fn rebuild_volatile_hardware(&mut self) {
        let cartridge = self.bus.take_cartridge();

        let mut bus = AddressBus::with_region(self.region);
        bus.io_mut()
            .port_mut(0)
            .unwrap()
            .connect_device(Box::new(Genesis3ButtonDevice::new()));
        bus.io_mut()
            .port_mut(1)
            .unwrap()
            .connect_device(Box::new(Genesis3ButtonDevice::new()));
        self.bus = bus;

        if let Some(cartridge) = cartridge {
            self.bus.set_cartridge(cartridge);
        }

        // `AddressBus::with_region` only sizes the audio chip clocks; the VDP
        // is constructed at its NTSC default, so re-apply the system region to
        // keep its scanline timing in lockstep (a no-op on NTSC).
        self.set_region(self.region);

        // Host-visible presentation state derived from the old machine.
        self.cached_framebuffer = Framebuffer::new(320, 224);
        self.mixed_waveform_buffer.clear();
        // A held button belongs to the outgoing session, never the incoming one.
        self.input_manager.clear_all_direct_overrides();
    }

    /// Power-cycle the console: the "turn it off and on again" that the
    /// reset button is not.
    ///
    /// [`EmulatorCore::reset`] emulates the Genesis *reset button*: it
    /// re-boots the CPUs but leaves work RAM, Z80 RAM and most of the VDP
    /// register file holding whatever the running game left there. This
    /// clears all of it while keeping the cartridge,
    /// ROM **and** battery-backed SRAM, plugged in, so a player who hits
    /// "Reset" on a game with a save file does not lose it.
    ///
    /// Distinct from [`Self::power_on_reset`], which rebuilds the whole
    /// emulator (dropping SRAM to a fresh image) because netplay needs two
    /// peers to converge on a byte-identical frame-0 baseline.
    pub fn hard_reset(&mut self) {
        self.rebuild_volatile_hardware();
        self.reset();
        // Re-apply the ROM's controller configuration, which lives on the
        // I/O ports the rebuild replaced.
        let rom = std::mem::take(&mut self.last_rom);
        if !rom.is_empty() {
            self.connect_controllers_for_rom(&rom);
        }
        self.last_rom = rom;
    }

    /// Deterministic hash of the full machine state, for netplay desync
    /// detection and the determinism harness.
    ///
    /// Hashes the same snapshot [`EmulatorCore::save_state`] serializes
    /// (every field [`EmulatorCore::load_state`] restores), with the
    /// non-deterministic, presentation-only metadata (wall-clock
    /// timestamp, host emulator version, cartridge title, screenshot)
    /// canonicalized away. Two emulators in the same logical state
    /// therefore hash equal even if they were created at different
    /// wall-clock times, and a save/load round-trip is hash-preserving
    /// by construction (the hash covers exactly the restored payload).
    pub fn state_hash(&self) -> u64 {
        use crate::system::savestate::SaveStateMetadata;

        // Canonical, timestamp-free metadata so the hash is a pure
        // function of deterministic machine state.
        let metadata = SaveStateMetadata {
            game_title: String::new(),
            rom_hash: String::new(),
            timestamp: String::new(),
            slot: 0,
            emulator_version: String::new(),
        };
        let state = self.build_save_state(metadata);
        let bytes = bincode::serialize(&state).unwrap_or_default();
        retro_core::fnv1a64(&bytes)
    }

    /// Load a savestate, refusing one captured against another ROM unless
    /// `check` is `RomCheck::AnyRom`. [`EmulatorCore::load_state`] is this
    /// with `RomCheck::SameRom` and its refusal as text; a host that asked
    /// the player first loads a state made on another build of the game
    /// (a translation in progress, say) with `RomCheck::AnyRom`. The
    /// refusal says whether the ROM was the reason.
    pub fn load_state_with_rom_check(
        &mut self,
        data: &[u8],
        check: retro_core::savestate::RomCheck,
    ) -> Result<(), retro_core::savestate::LoadStateError> {
        use crate::system::savestate::SystemSaveState;
        use crate::system::savestate_impl::decompress_and_deserialize;
        use retro_core::savestate::{
            check_rom_hash, deserialize_compressed, has_magic, LoadStateError, RomCheck,
        };

        // Two on-disk formats coexist:
        // - Envelope: 4-byte magic ("RLSS") + bincode + zstd, the format
        //   `save_state` writes.
        // - Legacy: bincode + zstd of `SystemSaveState` directly,
        //   with no magic prefix.
        // `has_magic` discriminates with a 4-byte memcmp, cheap and
        // unambiguous because the envelope magic is not a valid zstd
        // frame header.
        let state: SystemSaveState = if has_magic(data) {
            let envelope = deserialize_compressed::<SystemSaveState>(data)?;
            if let Some(cart) = self.bus.cartridge() {
                check_rom_hash(&envelope, &cart.info().rom_hash, check)?;
            }
            envelope.payload
        } else {
            let state = decompress_and_deserialize(data)?;
            // A legacy savestate names its ROM by hash and title; the
            // refusal names the titles, the only names it ever showed.
            if let (RomCheck::SameRom, Some(cart)) = (check, self.bus.cartridge()) {
                if !state.metadata.rom_hash.is_empty()
                    && cart.info().rom_hash != state.metadata.rom_hash
                {
                    return Err(LoadStateError::OtherRom {
                        state_rom: state.metadata.game_title.to_string(),
                        loaded_rom: cart.info().title.to_string(),
                    });
                }
            }
            state
        };

        // Apply state to all components
        self.cpu.from_savestate(&state.cpu68k);
        self.z80.from_savestate(&state.z80);
        self.bus.vdp_mut().from_savestate(&state.vdp);
        self.bus.psg_mut().from_savestate(&state.psg);
        self.bus.ym2612_mut().from_savestate(&state.ym2612);
        self.bus.io_mut().from_savestate(&state.io);
        self.bus.from_savestate(&state.bus);

        // Restore RAM
        if state.ram.len() == 65536 {
            self.bus
                .memory_mut()
                .work_ram
                .as_mut_slice()
                .copy_from_slice(&state.ram);
        }
        if state.z80_ram.len() == 8192 {
            self.bus
                .memory_mut()
                .z80_ram
                .as_mut_slice()
                .copy_from_slice(&state.z80_ram);
        }

        // Restore composable cartridge state: battery SRAM + typed
        // coprocessor states (rebinding chip clocks to the region master
        // clock). The pre-v3 opaque `cartridge` blob is ignored.
        let cart_mclk = self.region.master_clock();
        if let Some(cart) = self.bus.cartridge_mut() {
            if !state.cart_sram.is_empty() {
                cart.restore_sram(&state.cart_sram);
            }
            if !state.cart_coprocessors.is_empty() {
                cart.restore_coprocessors_savestate(&state.cart_coprocessors, cart_mclk);
            }
        }

        // Restore timing
        let t = &state.timing;
        self.frame_count = state.frame_count;
        self.master_cycles = t.master_cycles;
        // Restore the rate converter. If `t.audio_rate_converter` is
        // the default-constructed value (a fresh emulator's, before reset()
        // or set_region anchored it; a savestate from before the field cannot
        // be decoded at all, bincode reads every field), anchor a fresh
        // converter at the current `master_cycles`, equivalent to
        // `reset()` aligned to the restored emulation time, which
        // keeps subsequent `cycles_to_samples` deltas correct.
        // `t.audio_sample_accumulator` is intentionally ignored.
        let conv_state = &t.audio_rate_converter;
        if conv_state == &crate::system::savestate::SerializableConverterState::default() {
            self.audio_rate_converter.reset(self.master_cycles);
        } else {
            self.audio_rate_converter = retro_core::IntegerRateConverter::with_state(
                self.audio_sample_rate as u64,
                self.region.cpu_clock(),
                conv_state.reference_cycles,
                conv_state.samples_at_reference,
                conv_state.remainder,
            );
        }
        self.audio_enabled = t.audio_enabled;
        // Re-apply the host audio gate to the chips (it is not part of chip state).
        self.bus.ym2612_mut().set_rendering_enabled(t.audio_enabled);
        self.bus.psg_mut().set_rendering_enabled(t.audio_enabled);
        self.last_step_cycles = t.last_step_cycles;
        self.z80_irq_pending = t.z80_irq_pending;
        self.step_z80_target = t.step_z80_target;
        self.step_z80_executed = t.step_z80_executed;
        self.step_z80_credit_numer = t.step_z80_credit_numer;
        self.step_ym_cycles_clocked = t.step_ym_cycles_clocked;
        self.step_master_clocks_accumulated = t.step_master_clocks_accumulated;
        self.scanline_cycle_carry = t.scanline_cycle_carry;
        self.frame_cycle_overshoot = t.frame_cycle_overshoot;
        // Restore the refresh-slot target verbatim so a save/load round-trip
        // is bit-exact (netplay rollback depends on this). At a frame boundary
        // the target normally sits just AHEAD of master_cycles, but it can be
        // up to one long instruction BEHIND (the last instruction overshot it
        // and the next, which re-arms it via `+= 128`, has not run yet).
        // Recomputing it whenever target <= master_cycles would rewrite
        // exactly that legitimate behind-case, making the restored instance
        // charge a different bus-refresh penalty on its first instruction:
        // an immediate desync. `bus_refresh_target` boots at 128 and only ever
        // increments, so it is never 0 in a state this build produced (and a
        // savestate from before the field cannot be decoded: bincode reads every
        // field); only a 0 is re-anchored, to avoid `bus_refresh_penalty()`
        // charging a refresh every step until the stale target crawls back up
        // to master_cycles.
        self.bus_refresh_target = if t.bus_refresh_target == 0 {
            (self.master_cycles / 128) * 128 + 128
        } else {
            t.bus_refresh_target
        };

        // Clear transient buffers
        self.audio_ring.clear();
        self.audio_ring.reset_counters();

        Ok(())
    }

    /// Per-part FNV-1a-64 hashes of the canonical savestate payload, for
    /// netplay desync triage: when two instances (or a save/load
    /// round-trip) disagree on [`Self::state_hash`], this pinpoints which
    /// subsystem's serialized state diverged. Same canonicalization as
    /// `state_hash` (volatile metadata nulled), so the parts are a pure
    /// function of deterministic machine state.
    pub fn state_hash_parts(&self) -> Vec<(&'static str, u64)> {
        use crate::system::savestate::SaveStateMetadata;
        let s = self.build_save_state(SaveStateMetadata {
            game_title: String::new(),
            rom_hash: String::new(),
            timestamp: String::new(),
            slot: 0,
            emulator_version: String::new(),
        });
        let h = |bytes: Vec<u8>| retro_core::fnv1a64(&bytes);
        vec![
            (
                "cpu68k",
                h(bincode::serialize(&s.cpu68k).unwrap_or_default()),
            ),
            ("z80", h(bincode::serialize(&s.z80).unwrap_or_default())),
            ("vdp", h(bincode::serialize(&s.vdp).unwrap_or_default())),
            ("psg", h(bincode::serialize(&s.psg).unwrap_or_default())),
            (
                "ym2612",
                h(bincode::serialize(&s.ym2612).unwrap_or_default()),
            ),
            ("io", h(bincode::serialize(&s.io).unwrap_or_default())),
            ("bus", h(bincode::serialize(&s.bus).unwrap_or_default())),
            (
                "timing",
                h(bincode::serialize(&s.timing).unwrap_or_default()),
            ),
            ("ram", h(bincode::serialize(&s.ram).unwrap_or_default())),
            (
                "z80_ram",
                h(bincode::serialize(&s.z80_ram).unwrap_or_default()),
            ),
            (
                "cartridge",
                h(bincode::serialize(&s.cartridge).unwrap_or_default()),
            ),
            ("frame_count", s.frame_count),
        ]
    }

    /// Build the full machine snapshot shared by [`EmulatorCore::save_state`]
    /// (which wraps it in the cross-core envelope) and [`Self::state_hash`]
    /// (which hashes it). `metadata` is supplied by the caller so
    /// `save_state` can stamp the wall clock via `SaveStateMetadata::now`
    /// while `state_hash` passes a canonical timestamp-free value.
    fn build_save_state(
        &self,
        metadata: crate::system::savestate::SaveStateMetadata,
    ) -> crate::system::savestate::SystemSaveState {
        use crate::system::savestate::*;

        // Composable cartridge state: the battery-SRAM image plus each
        // coprocessor's typed savestate (keyed by chip_id). The legacy opaque
        // `cartridge` blob is retired, always written empty.
        let (cart_sram, cart_coprocessors) = self
            .bus
            .cartridge()
            .map(|c| (c.sram().to_vec(), c.coprocessors_savestate()))
            .unwrap_or_default();

        SystemSaveState {
            version: SAVE_STATE_VERSION,
            metadata,
            cpu68k: self.cpu.to_savestate(),
            z80: self.z80.to_savestate(),
            vdp: self.bus.vdp().to_savestate(),
            psg: self.bus.psg().to_savestate(),
            ym2612: self.bus.ym2612().to_savestate(),
            io: self.bus.io().to_savestate(),
            bus: self.bus.to_savestate(),
            timing: SystemTimingState {
                master_cycles: self.master_cycles,
                // Deprecated f64 accumulator: always written as 0.0;
                // ignored on load. See SystemTimingState docs.
                audio_sample_accumulator: 0.0,
                audio_enabled: self.audio_enabled,
                last_step_cycles: self.last_step_cycles,
                z80_irq_pending: self.z80_irq_pending,
                step_z80_target: self.step_z80_target,
                step_z80_executed: self.step_z80_executed,
                step_z80_credit_numer: self.step_z80_credit_numer,
                step_ym_cycles_clocked: self.step_ym_cycles_clocked,
                step_master_clocks_accumulated: self.step_master_clocks_accumulated,
                audio_rate_converter: SerializableConverterState {
                    reference_cycles: self.audio_rate_converter.reference_cycles(),
                    samples_at_reference: self.audio_rate_converter.samples_at_reference(),
                    remainder: self.audio_rate_converter.remainder(),
                },
                scanline_cycle_carry: self.scanline_cycle_carry,
                frame_cycle_overshoot: self.frame_cycle_overshoot,
                bus_refresh_target: self.bus_refresh_target,
            },
            ram: self.bus.memory().work_ram.as_slice().to_vec(),
            z80_ram: self.bus.memory().z80_ram.as_slice().to_vec(),
            cartridge: Vec::new(),
            cart_sram,
            cart_coprocessors,
            frame_count: self.frame_count,
            screenshot: None,
        }
    }

    /// Get the total number of CPU cycles executed.
    pub fn master_cycles(&self) -> u64 {
        self.master_cycles
    }

    /// Get reference to the CPU.
    pub fn cpu(&self) -> &M68k {
        &self.cpu
    }

    /// Get mutable reference to the CPU.
    pub fn cpu_mut(&mut self) -> &mut M68k {
        &mut self.cpu
    }

    /// Get reference to the Z80 CPU.
    pub fn z80(&self) -> &Z80 {
        &self.z80
    }

    /// Get mutable reference to the Z80 CPU.
    pub fn z80_mut(&mut self) -> &mut Z80 {
        &mut self.z80
    }

    /// Check if Z80 is in reset (reads state from bus, which is set by 68K writes to 0xA11200).
    pub fn z80_is_reset(&self) -> bool {
        self.bus.is_z80_reset()
    }

    /// Set Z80 reset state. In real hardware this is controlled by 68K writes
    /// to 0xA11200. This method provides a programmatic interface for tests
    /// and initialization.
    pub fn set_z80_reset(&mut self, reset: bool) {
        // Write to bus register 0xA11200: bit 0 = 0 means hold reset, 1 means release
        let value = if reset { 0x00 } else { 0x01 };
        self.bus.write(0xA11200, value);
        if reset {
            self.z80.pc = 0x0000;
            self.z80.halted = false;
        }
    }

    /// Check if 68K has requested Z80 bus (reads state from bus, set by 68K writes to 0xA11100).
    pub fn z80_bus_requested(&self) -> bool {
        self.bus.is_z80_bus_requested()
    }

    /// Set Z80 bus request state. In real hardware this is controlled by 68K
    /// writes to 0xA11100. This method provides a programmatic interface for
    /// tests and initialization.
    pub fn set_z80_bus_request(&mut self, request: bool) {
        let value = if request { 0x01 } else { 0x00 };
        self.bus.write(0xA11100, value);
    }

    /// Get reference to the bus.
    pub fn bus(&self) -> &AddressBus {
        &self.bus
    }

    /// Get mutable reference to the bus.
    pub fn bus_mut(&mut self) -> &mut AddressBus {
        &mut self.bus
    }

    /// Get reference to the input manager.
    pub fn input_manager(&self) -> &InputManager {
        &self.input_manager
    }

    /// Get mutable reference to the input manager.
    pub fn input_manager_mut(&mut self) -> &mut InputManager {
        &mut self.input_manager
    }

    // ===== Cartridge Save State Methods =====
    /// Serialize just the cartridge's persistent state, the battery-SRAM
    /// image plus every coprocessor's typed savestate, as a self-contained
    /// bincode blob, independent of the full system savestate. `frame_count`
    /// is accepted for API compatibility and not stored.
    pub fn get_cartridge_save_state(&self, frame_count: u64) -> Result<Vec<u8>, String> {
        let _ = frame_count;
        if let Some(cart) = self.bus.cartridge() {
            let payload: CartridgeSavePayload =
                (cart.sram().to_vec(), cart.coprocessors_savestate());
            bincode::serialize(&payload).map_err(|e| e.to_string())
        } else {
            Err("No cartridge loaded".to_string())
        }
    }

    /// Restore the cartridge-only state produced by
    /// [`get_cartridge_save_state`](Self::get_cartridge_save_state).
    pub fn load_cartridge_save_state(&mut self, data: &[u8]) -> Result<(), String> {
        let (sram, coprocessors): CartridgeSavePayload =
            bincode::deserialize(data).map_err(|e| e.to_string())?;
        let mclk = self.region.master_clock();
        if let Some(cart) = self.bus.cartridge_mut() {
            if !sram.is_empty() {
                cart.restore_sram(&sram);
            }
            cart.restore_coprocessors_savestate(&coprocessors, mclk);
            Ok(())
        } else {
            Err("No cartridge loaded".to_string())
        }
    }

    /// Get cartridge ROM hash (SHA-256)
    pub fn get_cartridge_hash(&self) -> Option<String> {
        self.bus.cartridge().map(|c| c.info().rom_hash.clone())
    }

    /// Get cartridge title
    pub fn get_cartridge_title(&self) -> Option<String> {
        self.bus.cartridge().map(|c| c.info().title.clone())
    }

    /// Check if cartridge SRAM is dirty
    pub fn is_cartridge_sram_dirty(&self) -> bool {
        self.bus.cartridge().is_some_and(|c| c.is_sram_dirty())
    }

    /// Clear cartridge SRAM dirty flag
    pub fn clear_cartridge_sram_dirty(&mut self) {
        if let Some(cart) = self.bus.cartridge_mut() {
            cart.clear_sram_dirty();
        }
    }

    /// Read memory for debugging, with no side effects: the cartridge, work
    /// RAM and Z80 RAM as [`AddressBus::peek_byte`] gives them, 0 for the I/O
    /// and VDP ports (a read of theirs would change the running game). VRAM,
    /// CRAM and VSRAM are not on the 68000's bus; the VDP's own calls read
    /// them.
    pub fn read_memory_debug(&mut self, addr: u32, size: u32) -> Vec<u8> {
        (0..size)
            .map(|i| self.bus.peek_byte(addr.wrapping_add(i)))
            .collect()
    }

    /// Write memory for debugging, a byte at a time through the bus's CPU path:
    /// a write to an I/O or VDP port acts as a CPU write would.
    pub fn write_memory_debug(&mut self, addr: u32, data: &[u8]) {
        for (i, &byte) in data.iter().enumerate() {
            self.bus.write(addr + i as u32, byte);
        }
    }

    /// Disassemble instructions starting at address.
    pub fn disassemble_at(&mut self, addr: u32, count: u32) -> Vec<Instruction> {
        use crate::cpu68k::disasm;

        let mut instructions = Vec::with_capacity(count as usize);
        let mut current_addr = addr;

        for _ in 0..count {
            let parts = disasm::disassemble_one_parts(&mut self.bus.peek_view(), current_addr);

            instructions.push(Instruction {
                address: current_addr,
                bytes: parts.bytes,
                mnemonic: parts.mnemonic,
                operands: parts.operands,
                comment: parts.comment,
            });

            current_addr += parts.bytes_consumed;
        }

        instructions
    }

    /// Disassemble Z80 instructions starting at address.
    ///
    /// Returns instructions with lowercase formatting for trace output:
    /// mnemonics and register names are lowercase, hex values stay uppercase,
    /// no space after commas. Example: "ld bc,$1FD9" instead of "LD BC, $1FD9".
    pub fn disassemble_z80_at(&mut self, addr: u32, count: u32) -> Vec<Instruction> {
        use crate::z80::disasm;

        let mut instructions = Vec::with_capacity(count as usize);
        let mut current_addr = addr as u16;

        let mut z80_bus = self.bus.create_z80_bus();
        for _ in 0..count {
            let (text, size) = disasm::disassemble_at(&mut z80_bus, current_addr);

            // Convert to lowercase trace format:
            // "LD BC, $1FD9" -> "ld bc,$1FD9"
            let trace_text = disasm::to_trace_format(&text);

            // Split mnemonic and operands
            let (mnemonic, operands) = if let Some(pos) = trace_text.find(' ') {
                (
                    trace_text[..pos].to_string(),
                    trace_text[pos + 1..].trim().to_string(),
                )
            } else {
                (trace_text.clone(), String::new())
            };

            // Collect raw bytes
            let mut bytes = Vec::with_capacity(size as usize);
            for i in 0..size {
                bytes.push(z80_bus.read(current_addr.wrapping_add(i) as u32));
            }

            instructions.push(Instruction {
                address: current_addr as u32,
                bytes,
                mnemonic,
                operands,
                comment: None,
            });

            current_addr = current_addr.wrapping_add(size);
        }

        instructions
    }

    /// Step the Z80 CPU one instruction (debug path, 68K frozen).
    fn step_z80(&mut self) -> StepResult {
        // Handle Z80 reset-release transition
        if self.bus.take_z80_reset_release() {
            self.z80.pc = 0x0000;
            self.z80.sp = 0xFFFF;
            self.z80.iff1 = false;
            self.z80.iff2 = false;
            self.z80.im = 0;
            self.z80.halted = false;
            self.z80.ei = 0;
        }

        // Can't step if Z80 is in reset or bus is requested by 68K
        if self.bus.is_z80_reset() || self.bus.is_z80_bus_requested() {
            return StepResult {
                cycles: 0,
                breakpoint_hit: false,
                instruction_executed: false,
            };
        }

        // Capture pre-execution state for trace and history
        let pre_pc = self.z80.pc;
        let z80_tracing = self.debugger.is_z80_tracing_enabled();
        let scanline = self.bus.vdp().timing().scanline;

        // Check for interrupts before stepping
        let z80_irq = self.z80_irq_pending;
        let mut z80_bus = self.bus.create_z80_bus();
        let (processed, irq_cycles) = self.z80.check_interrupts(&mut z80_bus, z80_irq, false);
        let mut total_cycles = 0u32;
        if processed {
            self.z80_irq_pending = false;
            total_cycles += irq_cycles;
        }

        // Read raw instruction bytes at PC (up to 4) and disassemble BEFORE step
        let mut raw_bytes = [0u8; 4];
        for i in 0..4u16 {
            raw_bytes[i as usize] = z80_bus.read(pre_pc.wrapping_add(i) as u32);
        }
        let (disasm_text, _) = crate::z80::disasm::disassemble_from_bytes(pre_pc, &raw_bytes);

        // Record Z80 trace BEFORE step (captures pre-execution state, matching run_frame)
        if z80_tracing {
            self.debugger
                .record_z80_trace(&self.z80, 0, scanline, raw_bytes);
        }

        // Coverage: mark the Z80 PC we are about to execute from.
        self.debugger.record_coverage_z80(pre_pc);

        // Execute one Z80 instruction
        let cycles = self.z80.step(&mut z80_bus);
        total_cycles += cycles;

        // Clock YM2612 and PSG proportionally
        let ym_cycles = total_cycles * 15 / 7;
        z80_bus.clock_ym2612(ym_cycles);
        z80_bus.clock_psg(total_cycles);

        // Record Z80 instruction history using pre-execution PC and disassembly
        {
            let trace = InstructionTrace {
                address: pre_pc as u32,
                instruction: disasm_text,
                registers_before: Vec::new(),
                cycle_count: total_cycles,
            };
            self.debugger.record_z80_history(trace);
        }

        // Check Z80 breakpoints
        let bp_hit = self.debugger.check_z80_breakpoint(self.z80.pc as u32);

        StepResult {
            cycles: total_cycles,
            breakpoint_hit: bp_hit,
            instruction_executed: true,
        }
    }

    /// Get known instruction boundaries before an address.
    /// Returns addresses in ascending order (oldest to newest).
    pub fn get_boundaries_before(&self, addr: u32, count: usize) -> Vec<u32> {
        self.debugger.get_boundaries_before(addr, count)
    }

    /// Disassemble using known instruction boundaries (bsnes-style).
    /// For addresses that are known boundaries, disassemble the actual instruction.
    /// For addresses between known boundaries, show DC.W with the raw bytes.
    /// `before_count`: how many instructions to show before `pc`
    /// `after_count`: how many instructions to show after (including) `pc`
    ///
    /// Deprecated in favour of `disassemble_window`, which uses the
    /// in-memory coverage bitmap instead of a global HashSet of
    /// observed instruction starts. Kept for the `disassembleAroundPc`
    /// WASM binding.
    pub fn disassemble_around_pc(
        &mut self,
        pc: u32,
        before_count: usize,
        after_count: usize,
    ) -> Vec<Instruction> {
        use crate::cpu68k::disasm;

        let mut instructions = Vec::new();

        // Get known boundaries before PC
        let boundaries_before = self.debugger.get_boundaries_before(pc, before_count);

        // Disassemble each known boundary before PC
        for &addr in &boundaries_before {
            let parts = disasm::disassemble_one_parts(&mut self.bus.peek_view(), addr);
            instructions.push(Instruction {
                address: addr,
                bytes: parts.bytes,
                mnemonic: parts.mnemonic,
                operands: parts.operands,
                comment: parts.comment,
            });
        }

        // Disassemble forward from PC
        let mut current_addr = pc;
        for _ in 0..after_count {
            let parts = disasm::disassemble_one_parts(&mut self.bus.peek_view(), current_addr);
            instructions.push(Instruction {
                address: current_addr,
                bytes: parts.bytes,
                mnemonic: parts.mnemonic,
                operands: parts.operands,
                comment: parts.comment,
            });
            current_addr += parts.bytes_consumed;
        }

        instructions
    }

    /// Bitmap-aware disassembly window around `pc` for the given CPU.
    ///
    /// Thin wrapper that builds a [`GenesisDisasmDriver`] and delegates
    /// to [`retro_core::debug::disassemble_window`]. The algorithm
    /// (backward probe + forward decode) lives in `retro-core` so
    /// other cores share it; only the CPU-specific decoding and the
    /// 68K-vs-Z80 probe-offset table stay here.
    ///
    /// `cpu_id`: 0 = 68K, 1 = Z80.
    pub fn disassemble_window(
        &mut self,
        cpu_id: usize,
        pc: u32,
        before_count: usize,
        after_count: usize,
    ) -> Vec<DisasmRow> {
        let mut driver = GenesisDisasmDriver { emu: self, cpu_id };
        retro_core::disassemble_window(&mut driver, pc, before_count, after_count)
    }
}

/// Adapter wiring [`GenesisEmulator`] into the generic
/// [`retro_core::DisasmDriver`] contract for one CPU at a time. Owned
/// briefly inside [`GenesisEmulator::disassemble_window`] so we can
/// borrow the emulator mutably across the (back + forward) walk.
struct GenesisDisasmDriver<'a> {
    emu: &'a mut GenesisEmulator,
    cpu_id: usize,
}

impl<'a> retro_core::DisasmDriver for GenesisDisasmDriver<'a> {
    fn probe_offsets(&self) -> &'static [u32] {
        probe_offsets(self.cpu_id)
    }

    fn is_covered(&self, addr: u32) -> bool {
        self.emu.debugger.is_covered(self.cpu_id, addr)
    }

    fn decode_at(&mut self, addr: u32) -> DisasmRow {
        decode_one(self.emu, self.cpu_id, addr)
    }
}

impl GenesisEmulator {
    /// Disassemble a single instruction for the given CPU and wrap it
    /// in a `Code`-kind `DisasmRow`. Decoder dispatch follows the
    /// `cpu_id` convention used everywhere else in this file
    /// (0 = 68K, 1 = Z80).
    #[allow(dead_code)] // kept for potential ad-hoc callers; window goes through GenesisDisasmDriver
    fn decode_at(&mut self, cpu_id: usize, addr: u32) -> DisasmRow {
        decode_one(self, cpu_id, addr)
    }

    /// Update the cached framebuffer from the VDP.
    fn update_cached_framebuffer(&mut self) {
        let vdp_fb = self.bus.get_vdp_framebuffer();
        let display_width = self.bus.get_vdp_display_width();
        let display_height = self.bus.get_vdp_display_height();

        // VDP framebuffer is always 320x224 internally, but display_width can be 256 or 320
        // We need to handle this mismatch carefully
        let vdp_internal_width = 320u32;
        let vdp_internal_height = 224u32;
        let expected_vdp_size = (vdp_internal_width * vdp_internal_height * 4) as usize;

        // Resize cached framebuffer to match display dimensions if needed
        if self.cached_framebuffer.width != display_width
            || self.cached_framebuffer.height != display_height
        {
            self.cached_framebuffer = Framebuffer::new(display_width, display_height);
        }

        // Safety check: ensure VDP framebuffer has expected size
        if vdp_fb.len() != expected_vdp_size {
            // VDP framebuffer has unexpected size, skip copy
            eprintln!(
                "Warning: VDP framebuffer size mismatch: {} vs expected {}",
                vdp_fb.len(),
                expected_vdp_size
            );
            return;
        }

        // Copy framebuffer data, handling potential size difference
        if display_width == vdp_internal_width {
            // Same width (H40 mode): direct copy
            let copy_size = self.cached_framebuffer.data.len().min(vdp_fb.len());
            self.cached_framebuffer.data[..copy_size].copy_from_slice(&vdp_fb[..copy_size]);
        } else {
            // Different width (H32 mode): copy row by row, cropping the extra pixels
            let src_stride = (vdp_internal_width * 4) as usize;
            let dst_stride = (display_width * 4) as usize;
            let rows = display_height as usize;

            for y in 0..rows {
                let src_start = y * src_stride;
                let dst_start = y * dst_stride;

                // Copy only the visible portion of each row
                if src_start + dst_stride <= vdp_fb.len()
                    && dst_start + dst_stride <= self.cached_framebuffer.data.len()
                {
                    self.cached_framebuffer.data[dst_start..dst_start + dst_stride]
                        .copy_from_slice(&vdp_fb[src_start..src_start + dst_stride]);
                }
            }
        }
    }

    /// Update debugger with current CPU state (both 68K and Z80).
    fn update_debugger_state(&mut self) {
        // Build CPU state for M68000 (CPU ID 0)
        let m68k_state = CpuState {
            cpu_id: 0,
            registers: vec![
                // Data registers (D0-D7)
                RegisterValue {
                    name: "D0".to_string(),
                    value: self.cpu.d[0] as u64,
                },
                RegisterValue {
                    name: "D1".to_string(),
                    value: self.cpu.d[1] as u64,
                },
                RegisterValue {
                    name: "D2".to_string(),
                    value: self.cpu.d[2] as u64,
                },
                RegisterValue {
                    name: "D3".to_string(),
                    value: self.cpu.d[3] as u64,
                },
                RegisterValue {
                    name: "D4".to_string(),
                    value: self.cpu.d[4] as u64,
                },
                RegisterValue {
                    name: "D5".to_string(),
                    value: self.cpu.d[5] as u64,
                },
                RegisterValue {
                    name: "D6".to_string(),
                    value: self.cpu.d[6] as u64,
                },
                RegisterValue {
                    name: "D7".to_string(),
                    value: self.cpu.d[7] as u64,
                },
                // Address registers (A0-A7)
                RegisterValue {
                    name: "A0".to_string(),
                    value: self.cpu.a[0] as u64,
                },
                RegisterValue {
                    name: "A1".to_string(),
                    value: self.cpu.a[1] as u64,
                },
                RegisterValue {
                    name: "A2".to_string(),
                    value: self.cpu.a[2] as u64,
                },
                RegisterValue {
                    name: "A3".to_string(),
                    value: self.cpu.a[3] as u64,
                },
                RegisterValue {
                    name: "A4".to_string(),
                    value: self.cpu.a[4] as u64,
                },
                RegisterValue {
                    name: "A5".to_string(),
                    value: self.cpu.a[5] as u64,
                },
                RegisterValue {
                    name: "A6".to_string(),
                    value: self.cpu.a[6] as u64,
                },
                RegisterValue {
                    name: "A7".to_string(),
                    value: self.cpu.a[7] as u64,
                },
                // Special registers
                RegisterValue {
                    name: "PC".to_string(),
                    value: self.cpu.pc as u64,
                },
                RegisterValue {
                    name: "SR".to_string(),
                    value: self.cpu.sr as u64,
                },
            ],
            pc: self.cpu.pc,
            flags: self.cpu.sr as u32, // Status register contains flags
        };

        // Build CPU state for Z80 (CPU ID 1)
        let z80_state = CpuState {
            cpu_id: 1,
            registers: vec![
                RegisterValue {
                    name: "A".to_string(),
                    value: self.z80.a as u64,
                },
                RegisterValue {
                    name: "F".to_string(),
                    value: self.z80.f as u64,
                },
                RegisterValue {
                    name: "B".to_string(),
                    value: self.z80.b as u64,
                },
                RegisterValue {
                    name: "C".to_string(),
                    value: self.z80.c as u64,
                },
                RegisterValue {
                    name: "D".to_string(),
                    value: self.z80.d as u64,
                },
                RegisterValue {
                    name: "E".to_string(),
                    value: self.z80.e as u64,
                },
                RegisterValue {
                    name: "H".to_string(),
                    value: self.z80.h as u64,
                },
                RegisterValue {
                    name: "L".to_string(),
                    value: self.z80.l as u64,
                },
                RegisterValue {
                    name: "PC".to_string(),
                    value: self.z80.pc as u64,
                },
                RegisterValue {
                    name: "SP".to_string(),
                    value: self.z80.sp as u64,
                },
                RegisterValue {
                    name: "IX".to_string(),
                    value: self.z80.ix as u64,
                },
                RegisterValue {
                    name: "IY".to_string(),
                    value: self.z80.iy as u64,
                },
            ],
            pc: self.z80.pc as u32,
            flags: self.z80.f as u32,
        };

        // Update debugger with both CPUs
        self.debugger.update_cpu_states(vec![m68k_state, z80_state]);
    }

    /// Refresh the 68K pending interrupt level from the current VDP state.
    ///
    /// Refresh the VINT interrupt level from VDP state (level-triggered).
    ///
    /// The Genesis VDP's /INT output is level-triggered:
    ///   /INT_ACTIVE = F_FLAG (vblank_pending) AND VINT_ENABLE (register 1 bit 5)
    ///
    /// The F flag is cleared by IACK (interrupt acknowledge) when the 68K takes
    /// the VINT, NOT by reading the VDP status register (MAME and Genesis Plus
    /// GX agree).
    fn refresh_vdp_interrupt_level(&mut self) {
        let vblank_pending = self.bus.vdp().timing().vblank_pending;
        let vint_enabled = (self.bus.vdp().registers()[1] & 0x20) != 0;
        // The F flag may be set while the 68K IRQ request is still being held
        // off part-way into line 224; don't assert level 6
        // until that delay has elapsed.
        let vint_active = vblank_pending && vint_enabled && !self.bus.vdp().vint_request_delayed();
        if vint_active {
            self.cpu.pending_irq_level = self.cpu.pending_irq_level.max(6);
        } else if self.cpu.pending_irq_level == 6 {
            self.cpu.pending_irq_level = 0;
        }
    }

    /// Handle interrupt acknowledge (IACK) after the 68K takes an interrupt.
    /// Clears the appropriate VDP flags based on the acknowledged interrupt level.
    /// (MAME and Genesis Plus GX clear them at the same point.)
    fn handle_interrupt_ack(&mut self, level: u8) {
        if level >= 6 {
            self.bus.vdp_mut().ack_vint_iack();
        }
    }

    /// Fast step: execute one 68K instruction with minimal overhead.
    /// Skips: disassembly, register snapshots, instruction history, boundary recording,
    /// breakpoint checks, and update_debugger_state(). Used by run_frame_fast() when
    /// no debug features are active.
    /// Returns the number of CPU cycles consumed (including FIFO wait).
    fn step_fast(&mut self) -> u32 {
        // Tick VDP FIFO drain based on previous step's cycles
        if self.last_step_cycles > 0 {
            let elapsed_mclks = (self.last_step_cycles as u64) * (self.region.cpu_divider() as u64);
            self.bus.vdp_mut().tick_fifo(elapsed_mclks);
        }

        // Process pending interrupts between instructions (68000 checks IPL
        // at every instruction boundary, not just at scanline boundaries).
        // This is critical for instructions like ANDI/ORI/EORI to SR and RTE
        // that change the interrupt mask mid-scanline.
        self.refresh_vdp_interrupt_level();
        let pending_level = self.cpu.pending_irq_level;
        let irq_cycles = self.cpu.process_pending_interrupts(&mut self.bus);
        if irq_cycles > 0 {
            self.handle_interrupt_ack(pending_level);
        }

        // Reset bus cycles and VDP wait state
        self.bus.cycles_executed = 0;
        self.bus.vdp_wait_cycles = 0;

        // Set YM2612 CPU cycle for timestamp-based busy flag
        self.bus.ym2612_mut().current_cpu_cycle = self.master_cycles;
        self.bus.m68k_cycle = self.master_cycles;

        // Record coverage BEFORE stepping so the bitmap reflects every
        // PC the CPU actually executed from, regardless of whether the
        // instruction succeeded or trapped.
        self.debugger.record_coverage_68k(self.cpu.pc);

        // Execute one CPU instruction
        let cycles = self.cpu.step(&mut self.bus);

        // Check if a VDP register write caused VINT re-assertion
        if self.bus.vdp().should_trigger_vint() {
            let vint_enabled = (self.bus.vdp().registers()[1] & 0x20) != 0;
            if vint_enabled && !self.bus.vdp().vint_request_delayed() {
                self.cpu.request_interrupt(6);
            }
            self.bus.vdp_mut().ack_vint();
        }

        // Add VDP FIFO wait cycles
        let fifo_wait = self.bus.vdp_wait_cycles;
        self.bus.vdp_wait_cycles = 0;

        // Add 68K stall cycles for Z80-bus ($A00000-$A0FFFF) accesses this step.
        let z80_wait = self.bus.z80_bus_wait_cycles;
        self.bus.z80_bus_wait_cycles = 0;

        let refresh = self.bus_refresh_penalty();
        let total_cycles = irq_cycles + cycles + fifo_wait + refresh + z80_wait;
        self.master_cycles += total_cycles as u64;

        // Accumulate master clocks for VDP timing
        let total_master_clocks = (total_cycles as u64) * (self.region.cpu_divider() as u64);
        self.step_master_clocks_accumulated += total_master_clocks;

        // Update DMA progress
        self.bus.vdp_mut().tick_dma(total_master_clocks);
        // Advance the deferred-VINT countdown (the request lands part-way into the line)
        self.bus.vdp_mut().tick_vint_delay(total_master_clocks);

        // Save total cycles for FIFO drain timing in next step
        self.last_step_cycles = total_cycles;

        total_cycles
    }

    /// Drain up to one scanline's worth of pending 68k-bus DMA stall and return
    /// it as 68K cycles to pre-charge this line's CPU budget. Real hardware
    /// freezes the 68000 for the whole 68k->VRAM/CRAM/VSRAM transfer;
    /// we charge at most one line at a time so a multi-line transfer freezes the
    /// CPU line-by-line while the beam advances, instead of stalling a whole
    /// frame at once. The remainder stays in the VDP and is drained on following
    /// lines (and frames). The global CPU clock advances by the drained amount so
    /// audio/beam timing stays aligned. Returns 0 when no DMA is pending.
    fn drain_dma_stall_for_line(&mut self) -> u32 {
        let line_mclks = self.region.master_clocks_per_scanline() as u64;
        let stall_mclks = self.bus.vdp_mut().drain_dma_stall_mclks(line_mclks);
        if stall_mclks == 0 {
            return 0;
        }
        let divider = (self.region.cpu_divider() as u64).max(1);
        let stall_68k = (stall_mclks / divider) as u32;
        self.master_cycles += stall_68k as u64;
        stall_68k
    }

    /// Arm 68K bus-access recording when a memory watchpoint is active or the
    /// instruction access log is on, so the just-stepped instruction's
    /// reads/writes are logged. Returns whether arming happened, which the
    /// caller passes to [`Self::drain_watchpoints`]. Otherwise the bus stays on
    /// its zero-overhead path.
    fn arm_watchpoints(&mut self) -> bool {
        let armed = self.debugger.has_watchpoints() || self.instruction_access_logging;
        if armed {
            self.bus.set_watch_armed(true);
        }
        armed
    }

    /// Drain the 68K bus accesses recorded during the just-executed instruction
    /// at `pc`, keep them in the instruction access log when it is on, and
    /// match them against memory watchpoints, disarming recording so later
    /// bus traffic (audio, DMA bookkeeping, sub-CPU) isn't attributed to the 68K.
    /// Returns whether any read/write watchpoint fired.
    fn drain_watchpoints(&mut self, armed: bool, pc: u32) -> bool {
        if !armed {
            return false;
        }
        let events = self.bus.take_watch_events();
        let dmas = self.bus.take_watch_dmas();
        let hit = self.debugger.has_watchpoints() && self.watchpoint_hit(&events);
        self.bus.set_watch_armed(false);
        if self.instruction_access_logging {
            self.instruction_access_log = InstructionAccessLog {
                pc: pc & 0x00FF_FFFF,
                accesses: events,
                dmas,
            };
        }
        hit
    }

    /// Whether the accesses one instruction made fire a read or write
    /// watchpoint.
    ///
    /// A word access is expanded to its covered byte addresses so a watch on
    /// either byte fires. Read watchpoints also see instruction/operand
    /// fetches because the bus cannot distinguish a prefetch from a data read;
    /// write watchpoints are exact (a fetch never writes). Prefer execute
    /// breakpoints for code and write/RAM watches for data.
    fn watchpoint_hit(&mut self, events: &[(u32, u8, bool, u16)]) -> bool {
        // Expand each word access to its covered bytes so single-address
        // and multi-byte watches both see every touched byte. The batch
        // check then fires each watch at most once for the instruction.
        let mut accesses: Vec<(u32, bool, u8)> = Vec::with_capacity(events.len() * 2);
        for &(addr, size, is_write, value) in events {
            for off in 0..size as u32 {
                // The recorded `value` is a byte (size 1) or big-endian
                // word (size 2); pick the byte that lives at this offset
                // so value-matched watchpoints compare the right one.
                let byte = if size == 2 && off == 0 {
                    (value >> 8) as u8
                } else {
                    (value & 0xFF) as u8
                };
                accesses.push((addr.wrapping_add(off), is_write, byte));
            }
        }
        // One bus borrow serves both the sized-watch value peek and any
        // conditional-expression evaluation: the expression context reads
        // memory through the same bus, so there's no separate peek closure.
        // `cpu` / `bus` / `debugger` are disjoint fields of `self`.
        let mut ctx = crate::expr_ctx::M68kExprContext::new(&self.cpu, &mut self.bus);
        self.debugger
            .should_break_watch_batch_ctx(&accesses, &mut ctx)
    }

    /// Evaluate execute breakpoints at `pc` on the 68K, threading a live
    /// expression context (registers + bus) so conditional breakpoints
    /// (`MatchSpec.expr`) can be checked. `cpu` / `bus` / `debugger` are
    /// disjoint fields, so the shared CPU borrow and mutable bus borrow in
    /// the context coexist with the mutable debugger borrow.
    fn check_execute_breakpoint(&mut self, pc: u32) -> bool {
        let mut ctx = crate::expr_ctx::M68kExprContext::new(&self.cpu, &mut self.bus);
        self.debugger
            .should_break_ctx(pc, BreakpointType::execute(), &mut ctx)
    }

    /// Lightweight debug step: execute one 68K instruction with breakpoint checking
    /// and optional trace recording, but WITHOUT the heavy per-instruction allocations
    /// (no disassembly, no register snapshots, no instruction history, no debugger state updates).
    /// Returns a StepResult indicating cycles consumed and whether a breakpoint was hit.
    fn step_debug_light(&mut self) -> StepResult {
        // Tick VDP FIFO drain based on previous step's cycles
        if self.last_step_cycles > 0 {
            let elapsed_mclks = (self.last_step_cycles as u64) * (self.region.cpu_divider() as u64);
            self.bus.vdp_mut().tick_fifo(elapsed_mclks);
        }

        self.refresh_vdp_interrupt_level();
        let pending_level = self.cpu.pending_irq_level;
        let irq_cycles = self.cpu.process_pending_interrupts(&mut self.bus);
        if irq_cycles > 0 {
            self.handle_interrupt_ack(pending_level);
            self.master_cycles += irq_cycles as u64;
            self.step_master_clocks_accumulated +=
                (irq_cycles as u64) * (self.region.cpu_divider() as u64);
        }

        if self.cpu.stopped {
            let idle_cycles = 4u32;
            self.master_cycles += idle_cycles as u64;
            self.step_master_clocks_accumulated +=
                (idle_cycles as u64) * (self.region.cpu_divider() as u64);
            self.last_step_cycles = idle_cycles;
            return StepResult {
                cycles: idle_cycles,
                breakpoint_hit: false,
                instruction_executed: false,
            };
        }

        let pc = self.cpu.pc;
        let opcode = self.bus.read_word(pc);

        // Breakpoint check (before execution)
        let breakpoint_hit_before = if self.last_bp_checked_pc == Some(pc) {
            false
        } else {
            self.check_execute_breakpoint(pc)
        };

        // Capture lightweight state for trace (stack-only, zero heap allocations)
        let sr_before = self.cpu.sr;
        let d_before = self.cpu.d;
        let a_before = self.cpu.a;
        let total_cycles_before = self.master_cycles;
        let scanline_before = self.bus.vdp().timing().scanline;

        self.bus.cycles_executed = 0;
        self.bus.vdp_wait_cycles = 0;
        self.bus.ym2612_mut().current_cpu_cycle = self.master_cycles;
        self.bus.m68k_cycle = self.master_cycles;

        self.debugger.record_coverage_68k(pc);

        let watch_armed = self.arm_watchpoints();
        let cycles = self.cpu.step(&mut self.bus);
        let watchpoint_hit = self.drain_watchpoints(watch_armed, pc);

        if self.bus.vdp().should_trigger_vint() {
            let vint_enabled = (self.bus.vdp().registers()[1] & 0x20) != 0;
            if vint_enabled && !self.bus.vdp().vint_request_delayed() {
                self.cpu.request_interrupt(6);
            }
            self.bus.vdp_mut().ack_vint();
        }

        let fifo_wait = self.bus.vdp_wait_cycles;
        self.bus.vdp_wait_cycles = 0;

        // Add 68K stall cycles for Z80-bus ($A00000-$A0FFFF) accesses this step.
        let z80_wait = self.bus.z80_bus_wait_cycles;
        self.bus.z80_bus_wait_cycles = 0;

        let refresh = self.bus_refresh_penalty();
        let total_cycles = irq_cycles + cycles + fifo_wait + refresh + z80_wait;
        self.master_cycles += total_cycles as u64;

        // Trace recording (conditional, uses pre-allocated buffer -- zero heap allocs)
        if self.debugger.is_68k_tracing_enabled() {
            use crate::debugger::TraceEntryBinary;
            let trace_entry = TraceEntryBinary::new(
                pc,
                sr_before,
                d_before,
                a_before,
                opcode,
                cycles as u16,
                total_cycles_before,
                scanline_before,
            );
            self.debugger.record_trace(&trace_entry);
        }

        // Call stack tracking (push/pop on existing Vec, no String allocations)
        if (opcode & 0xFFC0) == 0x4E80 {
            let instr_len = self.cpu.pc.wrapping_sub(pc);
            self.debugger.push_call(retro_core::types::StackFrame {
                call_address: pc,
                return_address: pc.wrapping_add(instr_len),
                function_name: None,
            });
        } else if opcode == 0x4E75 || opcode == 0x4E77 || opcode == 0x4E73 {
            self.debugger.pop_call();
        }

        let total_master_clocks = (total_cycles as u64) * (self.region.cpu_divider() as u64);
        self.step_master_clocks_accumulated += total_master_clocks;
        self.bus.vdp_mut().tick_dma(total_master_clocks);
        // Advance the deferred-VINT countdown (the request lands part-way into the line)
        self.bus.vdp_mut().tick_vint_delay(total_master_clocks);

        // Breakpoint check (after execution)
        let new_pc = self.cpu.pc;
        let breakpoint_hit_after = if new_pc != pc {
            self.last_bp_checked_pc = Some(new_pc);
            self.check_execute_breakpoint(new_pc)
        } else {
            self.last_bp_checked_pc = None;
            false
        };

        self.last_step_cycles = total_cycles;

        StepResult {
            cycles: total_cycles,
            breakpoint_hit: breakpoint_hit_before || breakpoint_hit_after || watchpoint_hit,
            instruction_executed: true,
        }
    }

    /// Advance the audio subsystem one frame, and, when `present`, mix and
    /// push the samples to the output ring. Uses pre-allocated buffers to avoid
    /// per-frame heap allocations.
    ///
    /// The first half (advancing `audio_rate_converter` and pulling each chip's
    /// samples, which drains its `clock_accumulator`) mutates *serialized
    /// machine state*, state the netplay desync hash (`state_hash`) covers, so
    /// it MUST run on every frame, audible or headless. If a headless
    /// rollback/catch-up frame skipped it, a re-simulated span would leave the
    /// converter + chip phase frozen where a live audible run advanced them, the
    /// two peers' hashes for that frame would differ, and the netplay desync
    /// check would fire a spurious full resync. The second half (the mix, the
    /// waveform-visualization ring, and the host audio ring) is pure
    /// presentation (none of it is serialized), so a headless frame returns
    /// before it: audio is generated but not rendered, exactly as rollback
    /// netcode requires.
    fn generate_and_mix_audio(&mut self, present: bool) {
        /// Soft limiter using tanh-like curve.
        /// Below threshold (0.8), signal passes through linearly.
        /// Above threshold, applies smooth compression to avoid hard clipping.
        /// Output is always in [-1.0, 1.0].
        #[inline]
        fn soft_limit(x: f32) -> f32 {
            const THRESHOLD: f32 = 0.8;
            if x.abs() <= THRESHOLD {
                x
            } else {
                let sign = x.signum();
                let excess = x.abs() - THRESHOLD;
                let compressed = THRESHOLD + (1.0 - THRESHOLD) * (1.0 - (-excess * 3.0).exp());
                sign * compressed.min(1.0)
            }
        }

        if !self.audio_enabled {
            return;
        }

        // Drift-free cycles->samples mapping. The converter is anchored
        // on `master_cycles`, so the delta between the previous
        // anchor's cumulative count and this frame's tells us exactly
        // how many stereo pairs to emit, bit-for-bit identical to a
        // single big-int compute done at the end of the session.
        let cumulative = self
            .audio_rate_converter
            .cycles_to_samples(self.master_cycles);
        let prev = self.audio_rate_converter.samples_at_reference();
        let samples_per_frame = cumulative.saturating_sub(prev) as usize;
        // Re-anchor immediately. `cumulative` already includes the
        // fractional remainder; carrying it forward inside `advance`
        // keeps the next frame's computation drift-free.
        self.audio_rate_converter
            .advance(self.master_cycles, cumulative);

        // Edge case: a paused / stepped emulator may report 0 delta
        // (no cycle advance since last call). Skip the heavy mix path
        // entirely: pushing an empty slice would be harmless but
        // wastes the chip generation work below.
        if samples_per_frame == 0 {
            return;
        }

        // Ensure pre-allocated buffers are large enough (resize only if needed)
        if self.psg_sample_buf.len() < samples_per_frame {
            self.psg_sample_buf.resize(samples_per_frame, 0.0);
        }
        if self.ym_sample_buf.len() < samples_per_frame * 2 {
            self.ym_sample_buf.resize(samples_per_frame * 2, 0.0);
        }
        if self.cart_sample_buf.len() < samples_per_frame * 2 {
            self.cart_sample_buf.resize(samples_per_frame * 2, 0.0);
        }

        // Zero the used portion of buffers
        for v in &mut self.psg_sample_buf[..samples_per_frame] {
            *v = 0.0;
        }
        for v in &mut self.ym_sample_buf[..samples_per_frame * 2] {
            *v = 0.0;
        }
        for v in &mut self.cart_sample_buf[..samples_per_frame * 2] {
            *v = 0.0;
        }

        // Generate samples from PSG and YM2612.
        // Both buffers were pre-zeroed so any samples beyond what each chip
        // produces are already silence (0.0).
        use retro_core::audio::AudioChip;
        let _psg_count = self.bus.psg_mut().generate_samples(
            &mut self.psg_sample_buf[..samples_per_frame],
            self.audio_sample_rate,
        );

        let ym_count = self.bus.ym2612_mut().generate_samples(
            &mut self.ym_sample_buf[..samples_per_frame * 2],
            self.audio_sample_rate,
        );
        let _ym_stereo_count = ym_count / 2;

        // Check if cartridge has audio
        let has_cart_audio = self
            .bus
            .cartridge()
            .and_then(|c| c.audio())
            .map(|a| a.is_enabled())
            .unwrap_or(false);

        // Generate cartridge audio if present
        let cart_count = if has_cart_audio {
            if let Some(cart) = self.bus.cartridge_mut() {
                if let Some(audio) = cart.audio_mut() {
                    audio.generate_audio(
                        &mut self.cart_sample_buf[..samples_per_frame * 2],
                        self.audio_sample_rate,
                    )
                } else {
                    0
                }
            } else {
                0
            }
        } else {
            0
        };

        // Everything above advanced serialized machine state (the converter and
        // each chip's `clock_accumulator`), so it ran on this frame regardless of
        // presentation. A headless frame stops here: the mix, the waveform ring,
        // and the host audio ring below are presentation-only and never enter the
        // netplay desync hash. See the fn doc for why this ordering is
        // load-bearing for rollback determinism.
        if !present {
            return;
        }

        // Mix PSG + YM2612 + Cartridge Audio (stereo interleaved: L, R, L, R, ...)
        // Always output exactly samples_per_frame to keep the audio buffer fill
        // rate matched to the video frame rate. Both sample buffers are pre-zeroed,
        // so if either chip produced slightly fewer samples, the tail is silence.
        let cart_stereo_count = cart_count / 2;
        let mix_count = samples_per_frame;

        for i in 0..mix_count {
            // PSG is mono - duplicate to L and R
            let psg = self.psg_sample_buf[i];

            // YM2612 is stereo interleaved
            let ym_left = self.ym_sample_buf[i * 2];
            let ym_right = self.ym_sample_buf[i * 2 + 1];

            // Cartridge audio (stereo or none)
            let (cart_left, cart_right) = if has_cart_audio && i < cart_stereo_count {
                let cl = self.cart_sample_buf[i * 2];
                let cr = self.cart_sample_buf[i * 2 + 1];
                let cart_volume = self
                    .bus
                    .cartridge()
                    .and_then(|c| c.audio())
                    .map(|a| a.get_volume())
                    .unwrap_or(1.0);
                (cl * cart_volume, cr * cart_volume)
            } else {
                (0.0, 0.0)
            };

            // Mix: PSG (mono->both) + YM2612 (stereo) + Cart (stereo)
            // PSG/FM balance: the PSG at full scale is about a third of the
            // FM at full scale (0.34, the ratio Genesis Plus GX's default
            // levels give). 0.25/0.75 gives 0.333, with max output = 1.0.
            let (mixed_left, mixed_right) = if has_cart_audio {
                (
                    soft_limit(psg * 0.20 + ym_left * 0.60 + cart_left * 0.20),
                    soft_limit(psg * 0.20 + ym_right * 0.60 + cart_right * 0.20),
                )
            } else {
                (
                    soft_limit(psg * 0.25 + ym_left * 0.75),
                    soft_limit(psg * 0.25 + ym_right * 0.75),
                )
            };

            // Reuse `cart_sample_buf` as a scratch staging area for
            // stereo-interleaved mixed output: it was zeroed at the top
            // of this function and we no longer need its original
            // contents past this point in the mix. Avoids a per-frame
            // Vec alloc.
            self.cart_sample_buf[i * 2] = mixed_left;
            self.cart_sample_buf[i * 2 + 1] = mixed_right;

            // Store mono mix for waveform visualization
            let mono = (mixed_left + mixed_right) * 0.5;
            if self.mixed_waveform_buffer.len() >= 2048 {
                self.mixed_waveform_buffer.pop_front();
            }
            self.mixed_waveform_buffer.push_back(mono);
        }

        // The ring is bounded; if the host is way behind (debugger
        // paused, tab backgrounded) `push_slice` quietly drops the
        // oldest samples and bumps `overflow_count`. Callers diagnose
        // backpressure through `audio_ring_overflows()`.
        self.audio_ring
            .push_slice(&self.cart_sample_buf[..mix_count * 2]);
    }

    /// Returns the exact 68K-cycle budget for the next scanline, carrying the
    /// fractional remainder across scanlines and frames.
    ///
    /// A scanline is `master_clocks_per_scanline / cpu_divider = 3420 / 7 =
    /// 488.571...` 68K cycles. Truncating to `488` loses `4/7` of a cycle per
    /// line (~150 cycles/frame, ~0.12% fast-forward drift). Instead we spend
    /// the integer base every line and accumulate the `3420 % 7 = 4` remainder
    /// in [`scanline_cycle_carry`](Self::scanline_cycle_carry), spending one
    /// extra cycle whenever it reaches a full `cpu_divider`. Over a frame this
    /// averages the exact `896,040 / 7` cycles of an NTSC frame, and the carry
    /// surviving across frames keeps the sub-frame phase aligned with hardware.
    fn next_scanline_cycles(&mut self) -> u32 {
        let mclk = self.region.master_clocks_per_scanline();
        let div = self.region.cpu_divider().max(1);
        let base = mclk / div;
        self.scanline_cycle_carry += mclk % div;
        if self.scanline_cycle_carry >= div {
            self.scanline_cycle_carry -= div;
            base + 1
        } else {
            base
        }
    }

    /// 68K DRAM-refresh bus-steal penalty, in 68K cycles, for the instruction
    /// about to be accounted. The Mega Drive steals the 68000 bus for DRAM
    /// refresh roughly every `128` CPU cycles, stalling the CPU `+2` cycles.
    /// It is checked once per instruction against the absolute cycle counter
    /// (the period and the penalty are the ones Genesis Plus GX uses).
    /// Returns `2` when `master_cycles` has reached the next refresh slot
    /// (advancing the slot), `0` otherwise. Folding the result into the
    /// instruction's `total_cycles` charges it to the line budget, `master_cycles`,
    /// and the VDP master-clock accumulator at once.
    fn bus_refresh_penalty(&mut self) -> u32 {
        const REFRESH_PERIOD_CYCLES: u64 = 128;
        const REFRESH_PENALTY_CYCLES: u32 = 2;
        if self.master_cycles >= self.bus_refresh_target {
            self.bus_refresh_target += REFRESH_PERIOD_CYCLES;
            REFRESH_PENALTY_CYCLES
        } else {
            0
        }
    }

    /// Advance every cartridge coprocessor to the current master cycle. Run
    /// once per frame from every tier so a coprocessor that runs decoupled
    /// from the 68000 (the SSP1601) makes progress even on the fast path.
    /// For every other chip it is a no-op.
    fn catch_up_cartridge(&mut self) {
        let mc = self.master_cycles;
        if let Some(cart) = self.bus.cartridge_mut() {
            cart.catch_up_chip(mc);
        }
    }

    /// Check the raster/interrupt event breakpoints for one scanline and
    /// return whether the run loop should pause. `vblank`/`vint`/`hint` say
    /// which interrupt-class events fired on this line; `Scanline` is checked
    /// on every line. Short-circuits to `false` (zero cost) unless an event or
    /// watch breakpoint is armed, so the debug tiers can call it unconditionally.
    fn check_frame_events(&mut self, scanline: u16, vblank: bool, vint: bool, hint: bool) -> bool {
        if !self.debugger.has_watchpoints() {
            return false;
        }
        let mut brk = self.debugger.check_event(GenesisEvent::Scanline(scanline));
        if vblank {
            brk |= self.debugger.check_event(GenesisEvent::VBlank);
        }
        if vint {
            brk |= self.debugger.check_event(GenesisEvent::VInt);
        }
        if hint {
            brk |= self.debugger.check_event(GenesisEvent::HInt);
        }
        brk
    }

    /// Shared fast-path frame body: runs a complete frame using
    /// step_fast() with no debug overhead. Skips debugger execution
    /// mode checks, breakpoints, disassembly, instruction history,
    /// register snapshots, and update_debugger_state().
    ///
    /// `present` gates the two presentation-only tail steps. When
    /// `false` (a headless netplay rollback/catch-up frame), audio
    /// synthesis (`generate_and_mix_audio`) and the host-visible
    /// framebuffer copy (`update_cached_framebuffer`) are skipped.
    /// Neither feeds back into machine state, so the scanline loop,
    /// Z80, PSG/YM2612 clocking, I/O, and the frame counter advance
    /// byte-identically whether or not the frame is presented.
    fn run_frame_fast_inner(&mut self, present: bool) {
        use retro_core::cpu::Clockable;

        // Sync input state from InputManager into I/O devices
        let p0 = self.input_manager.get_port_state(0);
        let p1 = self.input_manager.get_port_state(1);
        self.bus.io_mut().sync_input(p0, p1);

        let scanlines = self.region.scanlines();
        let has_rom = self.bus.cartridge().is_some();

        // Track 68K overshoot between scanlines. On real hardware, the 68K uses
        // a continuous cycle counter, so if the last instruction of scanline N
        // overshoots by X cycles, scanline N+1 has X fewer cycles available.
        // We carry the overshoot across frames too via frame_cycle_overshoot.
        let mut m68k_overshoot = self.frame_cycle_overshoot;

        for scanline in 0..scanlines {
            // Update VDP scanline
            self.bus.vdp_mut().scanline(scanline as u16);
            self.step_master_clocks_accumulated = 0;
            // Exact per-line 68K budget; carries the 4/7-cycle remainder.
            let cycles_per_scanline = self.next_scanline_cycles();

            // Check for VBLANK interrupt (VINT)
            if self.bus.vdp().should_trigger_vint() {
                // 68K: VINT is level 6 interrupt
                let vint_enabled = (self.bus.vdp().registers()[1] & 0x20) != 0;
                if vint_enabled && !self.bus.vdp().vint_request_delayed() {
                    self.cpu.request_interrupt(6);
                }
                // Z80: VBLANK also triggers Z80 maskable IRQ (same VDP /INT line)
                self.z80_irq_pending = true;
                self.bus.vdp_mut().ack_vint();
            }

            // Clear Z80 IRQ when no longer in VBLANK (level-triggered)
            if !self.bus.vdp().in_vblank() {
                self.z80_irq_pending = false;
            }

            // Check for HBLANK interrupt (HINT)
            if self.bus.vdp().should_trigger_hint() {
                self.cpu.request_interrupt(4);
                self.bus.vdp_mut().ack_hint();
            }

            // Refresh VINT level from VDP state (level-triggered)
            self.refresh_vdp_interrupt_level();

            // Process pending interrupts.
            // Interrupt processing cycles count toward the scanline budget: the
            // CPU has one cycle count for instructions and interrupts alike.
            // Without this, HINT-heavy games get ~7% extra 68K cycles.
            let pending_level = self.cpu.pending_irq_level;
            let irq_cycles = self.cpu.process_pending_interrupts(&mut self.bus);
            if irq_cycles > 0 {
                self.handle_interrupt_ack(pending_level);
                self.master_cycles += irq_cycles as u64;
            }

            // Interleaved 68K + Z80 + YM2612 execution for this scanline.
            // Running the Z80 after each 68K instruction ensures the Z80 can execute
            // even when the bus is only briefly free mid-scanline (e.g. Gaiares checks
            // a Z80 init flag shortly after releasing the Z80 from reset).
            //
            // Clock ratios (from master clock):
            //   Z80: master / 15, 68K/YM2612: master / 7
            //   So Z80_cycles = 68K_cycles * 7 / 15
            let z80_tracing = self.debugger.is_z80_tracing_enabled();
            let z80_scanline = self.bus.vdp().timing().scanline;
            let psg_target = self.region.master_clocks_per_scanline() / 15;

            // Interrupt cycles and previous overshoot count toward the scanline budget
            // (all of the CPU's time comes out of the same count).
            // A pending 68k-bus DMA freezes the CPU; drain up to one line of it
            // into this scanline's budget so the transfer meters line-by-line.
            let dma_stall = self.drain_dma_stall_for_line();
            let mut cycles_executed = irq_cycles + m68k_overshoot + dma_stall;
            let mut z80_target = 0u32; // absolute Z80 cycle target (monotonically increasing)
            let mut z80_credit_numer = 0u32; // fractional accumulator (numerator, /15)
            let mut z80_cycles_executed = 0u32;
            let mut ym_cycles_clocked = 0u32;
            let mut psg_cycles_clocked = 0u32; // PSG runs at master/15 regardless of Z80 bus state
            let mut psg_credit_numer = 0u32; // fractional accumulator (numerator, /15)
            let mut z80_irq = self.z80_irq_pending;

            while has_rom && cycles_executed < cycles_per_scanline {
                let step_cycles = self.step_fast();
                cycles_executed += step_cycles;

                // Spread the PSG's per-scanline budget across the line so register
                // writes resolve to ~1 output sample via Psg::catch_up instead of
                // collapsing to the line end. The PSG clocks at master/15 regardless
                // of the Z80 bus, so spend 7 numerator per 68K (master/7) cycle and
                // carry the /15 remainder within the line.
                psg_credit_numer += step_cycles * 7;
                let psg_step = psg_credit_numer / 15;
                psg_credit_numer %= 15;
                if psg_step > 0 {
                    self.bus.psg_mut().clock(psg_step);
                    psg_cycles_clocked += psg_step;
                }

                // Update HBLANK approximation based on 68K cycle position
                self.bus
                    .vdp_mut()
                    .set_hblank_approx(cycles_executed, cycles_per_scanline);

                // Handle Z80 reset-release transition per-instruction
                if self.bus.take_z80_reset_release() {
                    self.z80.pc = 0x0000;
                    self.z80.sp = 0xFFFF;
                    self.z80.iff1 = false;
                    self.z80.iff2 = false;
                    self.z80.im = 0;
                    self.z80.halted = false;
                    self.z80.ei = 0;
                }

                // Snapshot YM2612 progress before Z80 interleaving
                let ym_before_z80 = ym_cycles_clocked;

                // Accumulate absolute Z80 target when bus is free (fractional)
                if !self.bus.is_z80_reset() && !self.bus.is_z80_bus_requested() {
                    z80_credit_numer += step_cycles * 7;
                    z80_target += z80_credit_numer / 15;
                    z80_credit_numer %= 15;
                }

                // Run Z80 burst when target is sufficiently ahead of executed.
                // Using absolute target means overshoot from previous bursts
                // naturally prevents the Z80 from running until the target catches up.
                if z80_target > z80_cycles_executed + 3
                    && !self.bus.is_z80_reset()
                    && !self.bus.is_z80_bus_requested()
                {
                    let mut z80_bus = self.bus.create_z80_bus();
                    while z80_cycles_executed < z80_target {
                        if z80_tracing {
                            let pc = self.z80.pc;
                            let mut raw_bytes = [0u8; 4];
                            for i in 0..4u16 {
                                raw_bytes[i as usize] = z80_bus.read(pc.wrapping_add(i) as u32);
                            }
                            self.debugger
                                .record_z80_trace(&self.z80, 0, z80_scanline, raw_bytes);
                        }
                        let (processed, irq_cycles) =
                            self.z80.check_interrupts(&mut z80_bus, z80_irq, false);
                        if processed {
                            z80_irq = false;
                            z80_cycles_executed += irq_cycles;
                            let ym_c = (irq_cycles * 15 / 7)
                                .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                            z80_bus.clock_ym2612(ym_c);
                            ym_cycles_clocked += ym_c;
                        }
                        self.debugger.record_coverage_z80(self.z80.pc);
                        let zc = self.z80.step(&mut z80_bus);
                        z80_cycles_executed += zc;
                        let ym_c = (zc * 15 / 7)
                            .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                        z80_bus.clock_ym2612(ym_c);
                        ym_cycles_clocked += ym_c;
                    }
                }

                // Ensure YM2612 is clocked by at least the 68K instruction's cycle count
                // for timer accuracy and audio sample generation.
                let ym_target = ym_before_z80 + step_cycles;
                if ym_cycles_clocked < ym_target {
                    let ym_remaining = (ym_target - ym_cycles_clocked)
                        .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                    if ym_remaining > 0 {
                        self.bus.ym2612_mut().clock(ym_remaining);
                        ym_cycles_clocked += ym_remaining;
                    }
                }
            }

            // Top up Z80 to the scanline's proportional target.
            // z80_target from the accumulator may be slightly under psg_target
            // due to fractional rounding; use psg_target as the true max.
            if !self.bus.is_z80_reset() && !self.bus.is_z80_bus_requested() && has_rom {
                let z80_max = psg_target;
                if z80_cycles_executed < z80_max {
                    let mut z80_bus = self.bus.create_z80_bus();
                    while z80_cycles_executed < z80_max {
                        if z80_tracing {
                            let pc = self.z80.pc;
                            let mut raw_bytes = [0u8; 4];
                            for i in 0..4u16 {
                                raw_bytes[i as usize] = z80_bus.read(pc.wrapping_add(i) as u32);
                            }
                            self.debugger
                                .record_z80_trace(&self.z80, 0, z80_scanline, raw_bytes);
                        }
                        let (processed, irq_cycles) =
                            self.z80.check_interrupts(&mut z80_bus, z80_irq, false);
                        if processed {
                            z80_irq = false;
                            z80_cycles_executed += irq_cycles;
                            let ym_c = (irq_cycles * 15 / 7)
                                .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                            z80_bus.clock_ym2612(ym_c);
                            ym_cycles_clocked += ym_c;
                        }
                        self.debugger.record_coverage_z80(self.z80.pc);
                        let zc = self.z80.step(&mut z80_bus);
                        z80_cycles_executed += zc;
                        let ym_c = (zc * 15 / 7)
                            .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                        z80_bus.clock_ym2612(ym_c);
                        ym_cycles_clocked += ym_c;
                    }
                }
            }

            self.z80_irq_pending = z80_irq;

            // Save overshoot for next scanline (68K ran past the budget)
            m68k_overshoot = cycles_executed.saturating_sub(cycles_per_scanline);

            // Top up YM2612 for remaining scanline cycles
            if ym_cycles_clocked < cycles_per_scanline {
                self.bus
                    .ym2612_mut()
                    .clock(cycles_per_scanline - ym_cycles_clocked);
            }

            // Top up PSG to its exact per-scanline budget (covers the time the Z80
            // was halted/reset plus the line's fractional remainder), so the total
            // is still psg_target Z80 cycles per scanline.
            if psg_cycles_clocked < psg_target {
                self.bus.psg_mut().clock(psg_target - psg_cycles_clocked);
            }

            // I/O clocking
            self.bus.io_mut().clock(cycles_per_scanline);
        }

        // Carry the final scanline's instruction overshoot into the next frame.
        self.frame_cycle_overshoot = m68k_overshoot;

        // Advance cartridge coprocessors to end-of-frame (SSP1601 seam).
        self.catch_up_cartridge();

        // Advance the audio subsystem every frame: its converter + chip phase
        // are serialized machine state the netplay hash covers, so a headless
        // frame must advance them identically to an audible one (only the
        // sample mix + ring push inside are gated on `present`). The frame
        // counter is machine state and always advances; the host-visible
        // framebuffer copy is presentation-only and headless-skipped.
        self.generate_and_mix_audio(present);

        self.frame_count += 1;

        if present {
            self.update_cached_framebuffer();
        }
    }

    /// Fast frame execution with presentation (audio + framebuffer).
    /// This is the tier the WASM `runFrame` dispatch selects when no
    /// debug features are active.
    pub fn run_frame_fast(&mut self) {
        self.run_frame_fast_inner(true);
    }

    /// Headless frame for netplay rollback/catch-up: the full
    /// deterministic simulation with only the *presentation* outputs
    /// suppressed; the mixed audio is not pushed to the host ring and
    /// the host-visible framebuffer copy is skipped. Audio is still
    /// *synthesized* (the rate converter and each chip's sample-phase
    /// accumulators advance) because that is serialized machine state;
    /// suppressing it would make a re-simulated span hash differently
    /// from a live audible run and trip a spurious netplay resync.
    /// Produces the exact same machine state (including `frame_count`)
    /// as [`Self::run_frame_fast`], so re-simulating a mispredicted
    /// span leaves the emulator byte-identical to a live run.
    pub fn run_frame_headless(&mut self) {
        self.run_frame_fast_inner(false);
    }

    /// Lightweight debug frame execution: uses step_debug_light() which adds only
    /// breakpoint checking and trace recording on top of the fast path, without the
    /// heavy per-instruction allocations of the full debug path (no disassembly,
    /// no register snapshots, no instruction history, no update_debugger_state).
    /// This is the middle tier between run_frame_fast() and run_frame().
    pub fn run_frame_debug_light(&mut self) {
        use retro_core::cpu::Clockable;

        let p0 = self.input_manager.get_port_state(0);
        let p1 = self.input_manager.get_port_state(1);
        self.bus.io_mut().sync_input(p0, p1);

        let scanlines = self.region.scanlines();
        let has_rom = self.bus.cartridge().is_some();
        let mut m68k_overshoot = self.frame_cycle_overshoot;

        for scanline in 0..scanlines {
            self.bus.vdp_mut().scanline(scanline as u16);
            self.step_master_clocks_accumulated = 0;
            // Exact per-line 68K budget; carries the 4/7-cycle remainder.
            let cycles_per_scanline = self.next_scanline_cycles();

            let mut ev_vblank = false;
            let mut ev_vint = false;
            let mut ev_hint = false;
            if self.bus.vdp().should_trigger_vint() {
                ev_vblank = true;
                let vint_enabled = (self.bus.vdp().registers()[1] & 0x20) != 0;
                if vint_enabled && !self.bus.vdp().vint_request_delayed() {
                    self.cpu.request_interrupt(6);
                    ev_vint = true;
                }
                self.z80_irq_pending = true;
                self.bus.vdp_mut().ack_vint();
            }

            if !self.bus.vdp().in_vblank() {
                self.z80_irq_pending = false;
            }

            if self.bus.vdp().should_trigger_hint() {
                self.cpu.request_interrupt(4);
                self.bus.vdp_mut().ack_hint();
                ev_hint = true;
            }

            self.refresh_vdp_interrupt_level();

            let pending_level = self.cpu.pending_irq_level;
            let irq_cycles = self.cpu.process_pending_interrupts(&mut self.bus);
            if irq_cycles > 0 {
                self.handle_interrupt_ack(pending_level);
                self.master_cycles += irq_cycles as u64;
            }

            // Raster/interrupt event breakpoints. Checked after the interrupt is
            // taken (so a VInt break lands with PC at the handler entry) and
            // costs nothing unless an event/watch breakpoint is armed.
            if self.check_frame_events(scanline as u16, ev_vblank, ev_vint, ev_hint) {
                self.debugger.set_execution_mode(ExecutionMode::Paused);
                self.update_debugger_state();
                self.update_cached_framebuffer();
                return;
            }

            let z80_tracing = self.debugger.is_z80_tracing_enabled();
            let z80_scanline = self.bus.vdp().timing().scanline;
            let psg_target = self.region.master_clocks_per_scanline() / 15;

            // Drain up to one line of pending 68k-bus DMA stall (freezes the CPU).
            let dma_stall = self.drain_dma_stall_for_line();
            let mut cycles_executed = irq_cycles + m68k_overshoot + dma_stall;
            let mut z80_target = 0u32;
            let mut z80_credit_numer = 0u32;
            let mut z80_cycles_executed = 0u32;
            let mut ym_cycles_clocked = 0u32;
            let mut psg_cycles_clocked = 0u32; // PSG runs at master/15 regardless of Z80 bus state
            let mut psg_credit_numer = 0u32; // fractional accumulator (numerator, /15)
            let mut z80_irq = self.z80_irq_pending;

            while has_rom && cycles_executed < cycles_per_scanline {
                let step_result = self.step_debug_light();

                self.bus
                    .vdp_mut()
                    .set_hblank_approx(cycles_executed + step_result.cycles, cycles_per_scanline);

                if step_result.breakpoint_hit {
                    self.debugger.set_execution_mode(ExecutionMode::Paused);
                    self.z80_irq_pending = z80_irq;
                    self.update_debugger_state();
                    self.update_cached_framebuffer();
                    return;
                }

                cycles_executed += step_result.cycles;

                // Spread the PSG's per-scanline budget across the line so register
                // writes resolve to ~1 output sample via Psg::catch_up. The PSG runs
                // at master/15 independent of the Z80 bus, so spend 7 numerator per
                // 68K (master/7) cycle and carry the /15 remainder within the line.
                psg_credit_numer += step_result.cycles * 7;
                let psg_step = psg_credit_numer / 15;
                psg_credit_numer %= 15;
                if psg_step > 0 {
                    self.bus.psg_mut().clock(psg_step);
                    psg_cycles_clocked += psg_step;
                }

                if self.bus.take_z80_reset_release() {
                    self.z80.pc = 0x0000;
                    self.z80.sp = 0xFFFF;
                    self.z80.iff1 = false;
                    self.z80.iff2 = false;
                    self.z80.im = 0;
                    self.z80.halted = false;
                    self.z80.ei = 0;
                }

                let ym_before_z80 = ym_cycles_clocked;

                if !self.bus.is_z80_reset() && !self.bus.is_z80_bus_requested() {
                    z80_credit_numer += step_result.cycles * 7;
                    z80_target += z80_credit_numer / 15;
                    z80_credit_numer %= 15;
                }

                if z80_target > z80_cycles_executed + 3
                    && !self.bus.is_z80_reset()
                    && !self.bus.is_z80_bus_requested()
                {
                    let mut z80_bus = self.bus.create_z80_bus();
                    while z80_cycles_executed < z80_target {
                        if z80_tracing {
                            let pc = self.z80.pc;
                            let mut raw_bytes = [0u8; 4];
                            for i in 0..4u16 {
                                raw_bytes[i as usize] = z80_bus.read(pc.wrapping_add(i) as u32);
                            }
                            self.debugger
                                .record_z80_trace(&self.z80, 0, z80_scanline, raw_bytes);
                        }
                        let (processed, irq_cyc) =
                            self.z80.check_interrupts(&mut z80_bus, z80_irq, false);
                        if processed {
                            z80_irq = false;
                            z80_cycles_executed += irq_cyc;
                            let ym_c = (irq_cyc * 15 / 7)
                                .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                            z80_bus.clock_ym2612(ym_c);
                            ym_cycles_clocked += ym_c;
                        }
                        self.debugger.record_coverage_z80(self.z80.pc);
                        let zc = self.z80.step(&mut z80_bus);
                        z80_cycles_executed += zc;
                        let ym_c = (zc * 15 / 7)
                            .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                        z80_bus.clock_ym2612(ym_c);
                        ym_cycles_clocked += ym_c;
                    }
                }

                let ym_target = ym_before_z80 + step_result.cycles;
                if ym_cycles_clocked < ym_target {
                    let ym_remaining = (ym_target - ym_cycles_clocked)
                        .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                    if ym_remaining > 0 {
                        self.bus.ym2612_mut().clock(ym_remaining);
                        ym_cycles_clocked += ym_remaining;
                    }
                }
            }

            // Top up Z80 to the scanline's proportional target
            if !self.bus.is_z80_reset() && !self.bus.is_z80_bus_requested() && has_rom {
                let z80_max = psg_target;
                if z80_cycles_executed < z80_max {
                    let mut z80_bus = self.bus.create_z80_bus();
                    while z80_cycles_executed < z80_max {
                        if z80_tracing {
                            let pc = self.z80.pc;
                            let mut raw_bytes = [0u8; 4];
                            for i in 0..4u16 {
                                raw_bytes[i as usize] = z80_bus.read(pc.wrapping_add(i) as u32);
                            }
                            self.debugger
                                .record_z80_trace(&self.z80, 0, z80_scanline, raw_bytes);
                        }
                        let (processed, irq_cyc) =
                            self.z80.check_interrupts(&mut z80_bus, z80_irq, false);
                        if processed {
                            z80_irq = false;
                            z80_cycles_executed += irq_cyc;
                            let ym_c = (irq_cyc * 15 / 7)
                                .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                            z80_bus.clock_ym2612(ym_c);
                            ym_cycles_clocked += ym_c;
                        }
                        self.debugger.record_coverage_z80(self.z80.pc);
                        let zc = self.z80.step(&mut z80_bus);
                        z80_cycles_executed += zc;
                        let ym_c = (zc * 15 / 7)
                            .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                        z80_bus.clock_ym2612(ym_c);
                        ym_cycles_clocked += ym_c;
                    }
                }
            }

            self.z80_irq_pending = z80_irq;
            m68k_overshoot = cycles_executed.saturating_sub(cycles_per_scanline);

            if ym_cycles_clocked < cycles_per_scanline {
                self.bus
                    .ym2612_mut()
                    .clock(cycles_per_scanline - ym_cycles_clocked);
            }

            // Top up PSG to its exact per-scanline budget (covers the time the Z80
            // was halted/reset plus the line's fractional remainder).
            if psg_cycles_clocked < psg_target {
                self.bus.psg_mut().clock(psg_target - psg_cycles_clocked);
            }
            self.bus.io_mut().clock(cycles_per_scanline);
        }

        self.frame_cycle_overshoot = m68k_overshoot;

        // Advance cartridge coprocessors to end-of-frame (SSP1601 seam).
        self.catch_up_cartridge();

        self.generate_and_mix_audio(true);
        self.frame_count += 1;
        self.update_cached_framebuffer();
    }

    /// Profiled frame execution: identical to run_frame_fast() but measures time
    /// spent in each subsystem. Returns a FrameProfile with timing breakdowns.
    pub fn run_frame_profiled(&mut self) -> FrameProfile {
        use retro_core::cpu::Clockable;
        use std::time::Instant;

        // Sync input state from InputManager into I/O devices
        let p0 = self.input_manager.get_port_state(0);
        let p1 = self.input_manager.get_port_state(1);
        self.bus.io_mut().sync_input(p0, p1);

        let frame_start = Instant::now();
        let mut profile = FrameProfile::default();

        let scanlines = self.region.scanlines();
        let has_rom = self.bus.cartridge().is_some();
        let mut m68k_overshoot = self.frame_cycle_overshoot;

        for scanline in 0..scanlines {
            // VDP scanline (includes rendering)
            let t = Instant::now();
            self.bus.vdp_mut().scanline(scanline as u16);
            profile.vdp_ns += t.elapsed().as_nanos() as u64;

            self.step_master_clocks_accumulated = 0;
            // Exact per-line 68K budget; carries the 4/7-cycle remainder.
            let cycles_per_scanline = self.next_scanline_cycles();

            // Interrupt checks (cheap, lumped into "other")
            if self.bus.vdp().should_trigger_vint() {
                let vint_enabled = (self.bus.vdp().registers()[1] & 0x20) != 0;
                if vint_enabled && !self.bus.vdp().vint_request_delayed() {
                    self.cpu.request_interrupt(6);
                }
                self.bus.vdp_mut().ack_vint();
            }
            if self.bus.vdp().should_trigger_hint() {
                self.cpu.request_interrupt(4);
                self.bus.vdp_mut().ack_hint();
            }
            self.refresh_vdp_interrupt_level();
            let pending_level = self.cpu.pending_irq_level;
            let irq_cycles = self.cpu.process_pending_interrupts(&mut self.bus);
            if irq_cycles > 0 {
                self.handle_interrupt_ack(pending_level);
                self.master_cycles += irq_cycles as u64;
            }

            // Interleaved 68K + Z80 + YM2612 execution (same approach as run_frame_fast)
            let psg_target = self.region.master_clocks_per_scanline() / 15;
            let t = Instant::now();
            // Drain up to one line of pending 68k-bus DMA stall (freezes the CPU).
            let dma_stall = self.drain_dma_stall_for_line();
            let mut cycles_executed = irq_cycles + m68k_overshoot + dma_stall;
            let mut m68k_instructions = 0u32;
            let mut z80_target = 0u32;
            let mut z80_credit_numer = 0u32;
            let mut z80_cycles_executed = 0u32;
            let mut ym_cycles_clocked = 0u32;
            let mut z80_irq = self.z80_irq_pending;

            while has_rom && cycles_executed < cycles_per_scanline {
                let step_cycles = self.step_fast();
                cycles_executed += step_cycles;
                m68k_instructions += 1;
                self.bus
                    .vdp_mut()
                    .set_hblank_approx(cycles_executed, cycles_per_scanline);

                if self.bus.take_z80_reset_release() {
                    self.z80.pc = 0x0000;
                    self.z80.sp = 0xFFFF;
                    self.z80.iff1 = false;
                    self.z80.iff2 = false;
                    self.z80.im = 0;
                    self.z80.halted = false;
                    self.z80.ei = 0;
                }

                if !self.bus.is_z80_reset() && !self.bus.is_z80_bus_requested() {
                    z80_credit_numer += step_cycles * 7;
                    z80_target += z80_credit_numer / 15;
                    z80_credit_numer %= 15;
                }

                if z80_target > z80_cycles_executed + 3
                    && !self.bus.is_z80_reset()
                    && !self.bus.is_z80_bus_requested()
                {
                    let mut z80_bus = self.bus.create_z80_bus();
                    while z80_cycles_executed < z80_target {
                        let (processed, irq_cycles) =
                            self.z80.check_interrupts(&mut z80_bus, z80_irq, false);
                        if processed {
                            z80_irq = false;
                            z80_cycles_executed += irq_cycles;
                            let ym_c = (irq_cycles * 15 / 7)
                                .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                            z80_bus.clock_ym2612(ym_c);
                            ym_cycles_clocked += ym_c;
                        }
                        self.debugger.record_coverage_z80(self.z80.pc);
                        let zc = self.z80.step(&mut z80_bus);
                        z80_cycles_executed += zc;
                        let ym_c = (zc * 15 / 7)
                            .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                        z80_bus.clock_ym2612(ym_c);
                        ym_cycles_clocked += ym_c;
                        profile.z80_instructions += 1;
                    }
                }
            }

            // Top up Z80 to the scanline's proportional target
            if !self.bus.is_z80_reset() && !self.bus.is_z80_bus_requested() && has_rom {
                let z80_max = psg_target;
                if z80_cycles_executed < z80_max {
                    let mut z80_bus = self.bus.create_z80_bus();
                    while z80_cycles_executed < z80_max {
                        let (processed, irq_cycles) =
                            self.z80.check_interrupts(&mut z80_bus, z80_irq, false);
                        if processed {
                            z80_irq = false;
                            z80_cycles_executed += irq_cycles;
                            let ym_c = (irq_cycles * 15 / 7)
                                .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                            z80_bus.clock_ym2612(ym_c);
                            ym_cycles_clocked += ym_c;
                        }
                        self.debugger.record_coverage_z80(self.z80.pc);
                        let zc = self.z80.step(&mut z80_bus);
                        z80_cycles_executed += zc;
                        let ym_c = (zc * 15 / 7)
                            .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                        z80_bus.clock_ym2612(ym_c);
                        ym_cycles_clocked += ym_c;
                        profile.z80_instructions += 1;
                    }
                }
            }

            self.z80_irq_pending = z80_irq;

            m68k_overshoot = cycles_executed.saturating_sub(cycles_per_scanline);

            if ym_cycles_clocked < cycles_per_scanline {
                self.bus
                    .ym2612_mut()
                    .clock(cycles_per_scanline - ym_cycles_clocked);
            }

            self.bus.psg_mut().clock(psg_target);

            let interleaved_elapsed = t.elapsed().as_nanos() as u64;
            profile.m68k_ns += interleaved_elapsed / 2;
            profile.m68k_instructions += m68k_instructions as u64;
            profile.m68k_cycles += cycles_executed as u64;
            profile.z80_ns += interleaved_elapsed / 4;
            profile.psg_ns += interleaved_elapsed / 8;
            profile.ym2612_ns += interleaved_elapsed / 8;

            // I/O
            self.bus.io_mut().clock(cycles_per_scanline);
        }

        self.frame_cycle_overshoot = m68k_overshoot;

        // Audio generation
        let t = Instant::now();
        self.generate_and_mix_audio(true);
        profile.audio_mix_ns += t.elapsed().as_nanos() as u64;

        self.frame_count += 1;

        // Framebuffer update
        let t = Instant::now();
        self.update_cached_framebuffer();
        profile.framebuffer_ns += t.elapsed().as_nanos() as u64;

        profile.total_ns = frame_start.elapsed().as_nanos() as u64;
        profile
    }
}

/// Timing breakdown for a single frame's execution.
#[derive(Default, Clone)]
pub struct FrameProfile {
    pub total_ns: u64,
    pub m68k_ns: u64,
    pub z80_ns: u64,
    pub vdp_ns: u64,
    pub ym2612_ns: u64,
    pub psg_ns: u64,
    pub audio_mix_ns: u64,
    pub framebuffer_ns: u64,
    pub m68k_instructions: u64,
    pub m68k_cycles: u64,
    pub z80_instructions: u64,
}

impl FrameProfile {
    pub fn print_summary(&self, label: &str) {
        let total_us = self.total_ns as f64 / 1000.0;
        let m68k_us = self.m68k_ns as f64 / 1000.0;
        let z80_us = self.z80_ns as f64 / 1000.0;
        let vdp_us = self.vdp_ns as f64 / 1000.0;
        let ym2612_us = self.ym2612_ns as f64 / 1000.0;
        let psg_us = self.psg_ns as f64 / 1000.0;
        let audio_us = self.audio_mix_ns as f64 / 1000.0;
        let fb_us = self.framebuffer_ns as f64 / 1000.0;
        let accounted = m68k_us + z80_us + vdp_us + ym2612_us + psg_us + audio_us + fb_us;
        let other_us = total_us - accounted;

        let pct = |v: f64| -> f64 {
            if total_us > 0.0 {
                v / total_us * 100.0
            } else {
                0.0
            }
        };

        println!("  {}", label);
        println!(
            "  Total:       {:>10.1} us ({:.2} ms)",
            total_us,
            total_us / 1000.0
        );
        println!("  -------------------------------------------------------");
        println!(
            "  68K CPU:     {:>10.1} us  ({:>5.1}%)  [{} instr, {} cycles]",
            m68k_us,
            pct(m68k_us),
            self.m68k_instructions,
            self.m68k_cycles
        );
        println!(
            "  Z80 CPU:     {:>10.1} us  ({:>5.1}%)  [{} instr]",
            z80_us,
            pct(z80_us),
            self.z80_instructions
        );
        println!(
            "  VDP render:  {:>10.1} us  ({:>5.1}%)",
            vdp_us,
            pct(vdp_us)
        );
        println!(
            "  YM2612:      {:>10.1} us  ({:>5.1}%)",
            ym2612_us,
            pct(ym2612_us)
        );
        println!(
            "  PSG:         {:>10.1} us  ({:>5.1}%)",
            psg_us,
            pct(psg_us)
        );
        println!(
            "  Audio mix:   {:>10.1} us  ({:>5.1}%)",
            audio_us,
            pct(audio_us)
        );
        println!("  Framebuffer: {:>10.1} us  ({:>5.1}%)", fb_us, pct(fb_us));
        println!(
            "  Other/overhead: {:>7.1} us  ({:>5.1}%)",
            other_us,
            pct(other_us)
        );
    }
}

impl Default for GenesisEmulator {
    fn default() -> Self {
        Self::new()
    }
}

// Private helper methods for CPU stepping (used by EmulatorCore trait impl)
impl GenesisEmulator {
    /// Step the 68K CPU one instruction (full debug path).
    fn step_68k_internal(&mut self) -> StepResult {
        use retro_core::cpu::Clockable;

        // Advance VDP timing BEFORE instruction executes
        // This ensures VDP state is up-to-date when instruction reads VDP registers
        //
        // We use master clocks (3420 per scanline) instead of CPU cycles (488 per scanline)
        // to avoid fractional drift. CPU cycles/scanline = 3420/7 = 488.571..., which
        // causes ~0.3 scanlines of drift per frame when truncated to 488. Using master
        // clocks eliminates this entirely.
        let master_clocks_per_scanline = self.region.master_clocks_per_scanline() as u64;
        while self.step_master_clocks_accumulated >= master_clocks_per_scanline {
            self.step_master_clocks_accumulated -= master_clocks_per_scanline;
            // `run_frame` advances the beam once per outer iteration; burning
            // the credit here without a second `scanline()` keeps free-run
            // phase stable (see `frame_runner_owns_beam`).
            if self.frame_runner_owns_beam {
                continue;
            }
            let current_scanline = self.bus.vdp().timing().scanline;
            let next_scanline = (current_scanline + 1) % self.region.scanlines() as u16;
            self.bus.vdp_mut().scanline(next_scanline);

            // Check for VBLANK interrupt (VINT) when entering VBLANK
            // The VDP sets vint_irq_pending when transitioning to VBLANK scanline.
            // After requesting the interrupt, ack_vint() clears vint_irq_pending
            // to prevent the same VINT from being re-requested on every cycle.
            // The F flag (vblank_pending) is cleared by IACK when the 68K takes
            // the interrupt, not by status register reads.
            if self.bus.vdp().should_trigger_vint() {
                let vint_enabled = (self.bus.vdp().registers()[1] & 0x20) != 0;
                if vint_enabled && !self.bus.vdp().vint_request_delayed() {
                    self.cpu.request_interrupt(6);
                }
                self.z80_irq_pending = true;
                self.bus.vdp_mut().ack_vint();
            }

            // Clear Z80 IRQ when no longer in VBLANK (level-triggered)
            if !self.bus.vdp().in_vblank() {
                self.z80_irq_pending = false;
            }

            // Check for HBLANK interrupt (HINT)
            if self.bus.vdp().should_trigger_hint() {
                self.cpu.request_interrupt(4); // HBLANK = IRQ level 4
                self.bus.vdp_mut().ack_hint();
            }

            // Scanline boundary: top up YM2612 to full scanline, clock PSG and I/O,
            // then reset per-scanline accumulators.
            {
                let cycles_per_scanline = self.region.cycles_per_scanline();
                let psg_target = self.region.master_clocks_per_scanline() / 15;

                // Top up YM2612 for any remaining cycles in the scanline.
                if self.step_ym_cycles_clocked < cycles_per_scanline {
                    self.bus
                        .ym2612_mut()
                        .clock(cycles_per_scanline - self.step_ym_cycles_clocked);
                }
                self.step_ym_cycles_clocked = 0;

                // Clock PSG once per scanline
                self.bus.psg_mut().clock(psg_target);

                // I/O clocking
                self.bus.io_mut().clock(cycles_per_scanline);

                // Reset Z80 target/executed for the new scanline
                self.step_z80_target = 0;
                self.step_z80_executed = 0;
                self.step_z80_credit_numer = 0;
            }
        }

        // Refresh VINT level from VDP state (level-triggered)
        self.refresh_vdp_interrupt_level();

        // Process any pending interrupts BEFORE the next instruction
        let pending_level = self.cpu.pending_irq_level;
        let irq_cycles = self.cpu.process_pending_interrupts(&mut self.bus);
        if irq_cycles > 0 {
            self.handle_interrupt_ack(pending_level);
            self.master_cycles += irq_cycles as u64;
            self.step_master_clocks_accumulated +=
                (irq_cycles as u64) * (self.region.cpu_divider() as u64);
        }

        // If CPU is halted (STOP instruction), just consume idle cycles
        if self.cpu.stopped {
            let idle_cycles = 4u32;
            self.master_cycles += idle_cycles as u64;
            self.step_master_clocks_accumulated +=
                (idle_cycles as u64) * (self.region.cpu_divider() as u64);
            self.last_step_cycles = idle_cycles;
            return StepResult {
                cycles: idle_cycles,
                breakpoint_hit: false,
                instruction_executed: false,
            };
        }

        // Capture PC AFTER interrupt processing (interrupt may have changed PC)
        let pc = self.cpu.pc;

        // Read opcode for call stack tracking
        // Note: This reads from bus but we'll reset cycles before cpu.step()
        let opcode = self.bus.read_word(pc);

        // Check for execute breakpoint at current PC (before execution)
        // But skip if we already checked this PC in the previous step's "after" check
        let breakpoint_hit_before = if self.last_bp_checked_pc == Some(pc) {
            false // Already counted in previous step
        } else {
            self.check_execute_breakpoint(pc)
        };

        // Disassemble current instruction for history
        // Note: This also reads from bus but we'll reset cycles before cpu.step()
        use crate::cpu68k::disasm;
        let (disasm_str, instr_bytes) = disasm::disassemble_one(&mut self.bus, pc);

        // Capture registers before execution
        let registers_before = vec![
            RegisterValue {
                name: "PC".to_string(),
                value: self.cpu.pc as u64,
            },
            RegisterValue {
                name: "SR".to_string(),
                value: self.cpu.sr as u64,
            },
        ];

        // Capture CPU and VDP state BEFORE execution for trace
        // (SR, D registers, A registers, timing must be captured before step modifies them)
        let sr_before = self.cpu.sr;
        let d_before = self.cpu.d;
        let a_before = self.cpu.a;
        let total_cycles_before = self.master_cycles;
        let scanline_before = self.bus.vdp().timing().scanline;

        // Tick VDP FIFO: drain entries based on master clocks elapsed since last instruction.
        // This must happen before cpu.step() so the FIFO state is current when the CPU
        // accesses VDP ports. We use the previous step's total cycles (including any
        // IRQ processing cycles that were added to step_master_clocks_accumulated above).
        if self.last_step_cycles > 0 {
            let elapsed_mclks = (self.last_step_cycles as u64) * (self.region.cpu_divider() as u64);
            self.bus.vdp_mut().tick_fifo(elapsed_mclks);
        }

        // Reset bus cycles and VDP wait state AFTER all pre-step reads (opcode, disassembly)
        // This ensures only the actual instruction execution cycles are counted
        self.bus.cycles_executed = 0;
        self.bus.vdp_wait_cycles = 0;

        // Set YM2612 CPU cycle for timestamp-based busy flag.
        // Uses pre-instruction cycle count, matching MAME where machine().time()
        // during a bus access reflects the time before the instruction started.
        self.bus.ym2612_mut().current_cpu_cycle = self.master_cycles;
        self.bus.m68k_cycle = self.master_cycles;

        // Record coverage BEFORE stepping so the bitmap mirrors the
        // step_fast / step_debug_light paths and stays consistent
        // regardless of which execution mode the user landed in.
        self.debugger.record_coverage_68k(pc);

        // Execute one CPU instruction
        let watch_armed = self.arm_watchpoints();
        let cycles = self.cpu.step(&mut self.bus);
        let watchpoint_hit = self.drain_watchpoints(watch_armed, pc);

        // Check if a VDP register write during the instruction caused VINT to be
        // re-asserted (e.g., enabling VINT while F flag was already set from a
        // previous VBlank). On real hardware, the VDP's /INT output is combinatorial:
        //   /INT = F_FLAG AND VINT_ENABLE
        // So enabling VINT while F flag is set immediately asserts the interrupt line.
        if self.bus.vdp().should_trigger_vint() {
            let vint_enabled = (self.bus.vdp().registers()[1] & 0x20) != 0;
            if vint_enabled && !self.bus.vdp().vint_request_delayed() {
                self.cpu.request_interrupt(6);
            }
            self.bus.vdp_mut().ack_vint();
        }

        // Add VDP FIFO wait cycles (CPU stalled when FIFO is full during active display)
        let fifo_wait = self.bus.vdp_wait_cycles;
        self.bus.vdp_wait_cycles = 0;

        // Add 68K stall cycles for Z80-bus ($A00000-$A0FFFF) accesses this step.
        let z80_wait = self.bus.z80_bus_wait_cycles;
        self.bus.z80_bus_wait_cycles = 0;

        let refresh = self.bus_refresh_penalty();
        let total_cycles = cycles + fifo_wait + refresh + z80_wait;
        self.master_cycles += total_cycles as u64;

        // Z80 interleaving: run Z80 proportional to 68K cycles when bus is free.
        // This ensures the Z80 executes between 68K instructions, allowing games
        // that check Z80 status flags mid-scanline (e.g. Gaiares) to work.
        let ym_before_z80 = self.step_ym_cycles_clocked;
        {
            // Handle Z80 reset-release transition (per-instruction, not per-scanline)
            if self.bus.take_z80_reset_release() {
                self.z80.pc = 0x0000;
                self.z80.sp = 0xFFFF;
                self.z80.iff1 = false;
                self.z80.iff2 = false;
                self.z80.im = 0;
                self.z80.halted = false;
                self.z80.ei = 0;
            }

            // Accumulate absolute Z80 target from this 68K instruction
            // Z80 clock = master / 15, 68K clock = master / 7
            // So Z80 cycles = 68K cycles * 7 / 15
            if !self.bus.is_z80_reset() && !self.bus.is_z80_bus_requested() {
                self.step_z80_credit_numer += total_cycles * 7;
                self.step_z80_target += self.step_z80_credit_numer / 15;
                self.step_z80_credit_numer %= 15;
            }

            // Run Z80 when target is sufficiently ahead of executed.
            // Absolute target means overshoot from previous bursts naturally
            // prevents Z80 from running until the target catches up.
            if self.step_z80_target > self.step_z80_executed + 3
                && !self.bus.is_z80_reset()
                && !self.bus.is_z80_bus_requested()
                && self.bus.cartridge().is_some()
            {
                let mut z80_irq = self.z80_irq_pending;
                let cycles_per_scanline = self.region.cycles_per_scanline();
                let ym_cap = ym_before_z80 + total_cycles;
                let mut z80_bus = self.bus.create_z80_bus();

                while self.step_z80_executed < self.step_z80_target {
                    let (processed, irq_cycles) =
                        self.z80.check_interrupts(&mut z80_bus, z80_irq, false);
                    if processed {
                        z80_irq = false;
                        self.step_z80_executed += irq_cycles;
                        let ym_c = (irq_cycles * 15 / 7)
                            .min(cycles_per_scanline.saturating_sub(self.step_ym_cycles_clocked))
                            .min(ym_cap.saturating_sub(self.step_ym_cycles_clocked));
                        z80_bus.clock_ym2612(ym_c);
                        self.step_ym_cycles_clocked += ym_c;
                    }
                    self.debugger.record_coverage_z80(self.z80.pc);
                    let zc = self.z80.step(&mut z80_bus);
                    self.step_z80_executed += zc;
                    let ym_c = (zc * 15 / 7)
                        .min(cycles_per_scanline.saturating_sub(self.step_ym_cycles_clocked))
                        .min(ym_cap.saturating_sub(self.step_ym_cycles_clocked));
                    z80_bus.clock_ym2612(ym_c);
                    self.step_ym_cycles_clocked += ym_c;
                }

                self.z80_irq_pending = z80_irq;
            }

            // Ensure YM2612 is clocked by at least the 68K instruction's cycle count.
            // The YM2612 has its own clock (master/7, same as 68K) independent of the Z80.
            // When Z80 is running, it partially clocks the YM2612; we fill in the remainder.
            // When Z80 is held/reset, we clock the full amount. Without this, the busy flag
            // only decrements at scanline boundaries in bulk, causing games that poll
            // YM2612 status with Z80 bus held to see stale values.
            {
                let cycles_per_scanline = self.region.cycles_per_scanline();
                let ym_target = ym_before_z80 + total_cycles;
                if self.step_ym_cycles_clocked < ym_target {
                    let ym_remaining = (ym_target - self.step_ym_cycles_clocked)
                        .min(cycles_per_scanline.saturating_sub(self.step_ym_cycles_clocked));
                    if ym_remaining > 0 {
                        self.bus.ym2612_mut().clock(ym_remaining);
                        self.step_ym_cycles_clocked += ym_remaining;
                    }
                }
            }
        }

        // Record trace entry (if tracing is enabled)
        // Uses state captured BEFORE execution to match reference trace format
        if self.debugger.is_68k_tracing_enabled() {
            use crate::debugger::TraceEntryBinary;
            let trace_entry = TraceEntryBinary::new(
                pc,
                sr_before,
                d_before,
                a_before,
                opcode,
                cycles as u16,
                total_cycles_before,
                scanline_before,
            );
            self.debugger.record_trace(&trace_entry);
        }

        // Call stack tracking
        // JSR: 0x4E80-0x4EBF (various addressing modes)
        if (opcode & 0xFFC0) == 0x4E80 {
            // JSR - push return address to call stack
            let return_addr = pc.wrapping_add(instr_bytes);
            self.debugger.push_call(retro_core::types::StackFrame {
                call_address: pc,
                return_address: return_addr,
                function_name: None,
            });
        }
        // RTS (0x4E75) / RTR (0x4E77) / RTE (0x4E73): all three pop
        // the topmost call-stack frame, so we treat them uniformly.
        else if matches!(opcode, 0x4E75 | 0x4E77 | 0x4E73) {
            self.debugger.pop_call();
        }

        // Record instruction boundary (for bsnes-style disassembly)
        self.debugger.record_instruction_boundary(pc);

        // Record instruction in history
        let trace = InstructionTrace {
            address: pc,
            instruction: disasm_str,
            registers_before,
            cycle_count: cycles,
        };
        self.debugger.record_instruction(trace);

        // Accumulate master clocks for VDP timing (will be processed at start of next step)
        // Convert total CPU cycles (instruction + refresh penalty + FIFO wait) to master clocks
        // by multiplying by CPU divider (7). This includes bus contention effects.
        let total_master_clocks = (total_cycles as u64) * (self.region.cpu_divider() as u64);
        self.step_master_clocks_accumulated += total_master_clocks;

        // Update DMA progress: the VDP DMA continues in the background while
        // the CPU is running (for VRAM Fill and VRAM Copy - the CPU is NOT halted).
        // We tick the DMA progress by the same master clocks the CPU consumed.
        self.bus.vdp_mut().tick_dma(total_master_clocks);
        // Advance the deferred-VINT countdown (the request lands part-way into the line)
        self.bus.vdp_mut().tick_vint_delay(total_master_clocks);

        // Update debugger state AFTER step to reflect new register values
        self.update_debugger_state();

        // Check for breakpoint at NEW PC after execution (only if different from old PC)
        let new_pc = self.cpu.pc;
        let breakpoint_hit_after = if new_pc != pc {
            // Remember we checked this PC for next step
            self.last_bp_checked_pc = Some(new_pc);
            self.check_execute_breakpoint(new_pc)
        } else {
            self.last_bp_checked_pc = None;
            false
        };

        // Save total cycles for FIFO drain timing in next step
        self.last_step_cycles = total_cycles;

        StepResult {
            // Report the full charge (instruction + FIFO stall + DRAM refresh),
            // not the bare instruction cycles. run_frame() accumulates this
            // against the per-scanline budget; charging only `cycles` would let
            // the refresh/FIFO time run "for free", fitting ~1.6% extra
            // instructions per frame and drifting the CPU ahead of the beam
            // (which desyncs Contra's attract-mode demo). step_fast() and
            // step_debug_light() both already return total_cycles, so this keeps
            // the three execution tiers paced identically.
            cycles: total_cycles,
            breakpoint_hit: breakpoint_hit_before || breakpoint_hit_after || watchpoint_hit,
            instruction_executed: true,
        }
    }
}

impl EmulatorCore for GenesisEmulator {
    fn system_info(&self) -> &SystemInfo {
        &self.system_info
    }

    fn reset(&mut self) {
        // Reset debugger state first (ensures clean execution mode)
        self.debugger.set_execution_mode(ExecutionMode::Paused);
        self.debugger.clear_instruction_boundaries();
        self.last_bp_checked_pc = None;

        // Reset 68000 CPU
        self.cpu.reset(&mut self.bus);

        // Reset Z80 CPU
        self.z80 = Z80::new();
        // Put Z80 in reset state (write 0 to 0xA11200) and release bus (write 0 to 0xA11100)
        self.bus.write(0xA11200, 0x00); // Hold Z80 in reset
        self.bus.write(0xA11100, 0x00); // Release bus request

        // Reset VDP
        self.bus.vdp_mut().reset();

        // Reset PSG
        self.bus.psg_mut().reset();

        // Reset YM2612
        self.bus.ym2612_mut().reset();

        // Reset the cartridge: clear latches/SRAM-enable and re-anchor every
        // coprocessor's clock to the region master clock.
        let cart_mclk = self.region.master_clock();
        if let Some(cart) = self.bus.cartridge_mut() {
            cart.reset();
            cart.reset_chip(cart_mclk);
        }

        // Reset counters
        self.frame_count = 0;
        // MAME shows totalcycles=0x22 (34) after hard reset, before first instruction
        // This represents the reset sequence cycles. We match this for trace compatibility.
        self.master_cycles = 34;
        // Convert initial CPU cycles to master clocks: 34 * 7 = 238 master clocks
        self.step_master_clocks_accumulated = 34 * 7;
        self.frame_runner_owns_beam = false;
        self.scanline_cycle_carry = 0;
        self.frame_cycle_overshoot = 0;
        self.bus_refresh_target = 128;
        self.last_step_cycles = 0;
        self.z80_irq_pending = false;
        self.running = true;

        // Drop any queued samples (this is a real reset, not
        // mid-stream: the host is expected to drain after) and
        // re-anchor the rate converter at the new master_cycles
        // baseline (34, set above).
        self.audio_ring.clear();
        self.audio_ring.reset_counters();
        self.audio_rate_converter.reset(self.master_cycles);

        // Leave the reset state in the debugger, so a host that reads it
        // before the first step or frame (the platform opens its debugger
        // right after a load) shows the reset vector instead of nothing.
        // It reads the registers only: nothing here runs or changes state.
        self.update_debugger_state();
    }

    fn step(&mut self) -> StepResult {
        self.step_cpu(self.focused_cpu)
    }

    fn step_cpu(&mut self, cpu_id: usize) -> StepResult {
        match cpu_id {
            0 => self.step_68k_internal(),
            1 => self.step_z80(),
            _ => StepResult {
                cycles: 0,
                breakpoint_hit: false,
                instruction_executed: false,
            },
        }
    }

    fn focused_cpu(&self) -> usize {
        self.focused_cpu
    }

    fn set_focused_cpu(&mut self, cpu_id: usize) {
        if cpu_id < self.system_info.cpus.len() {
            self.focused_cpu = cpu_id;
        }
    }

    fn disassemble_cpu(&mut self, cpu_id: usize, addr: u32, count: u32) -> Vec<Instruction> {
        match cpu_id {
            0 => Self::disassemble_at(self, addr, count),
            1 => Self::disassemble_z80_at(self, addr, count),
            _ => Vec::new(),
        }
    }

    fn run_frame(&mut self) {
        use retro_core::cpu::Clockable;

        // Sync input state from InputManager into I/O devices
        let p0 = self.input_manager.get_port_state(0);
        let p1 = self.input_manager.get_port_state(1);
        self.bus.io_mut().sync_input(p0, p1);

        let scanlines = self.region.scanlines();
        let mut m68k_overshoot = self.frame_cycle_overshoot;

        // Own the beam for this frame so step_68k_internal does not also
        // call scanline() when a 489-cycle line crosses 3420 mclks.
        self.frame_runner_owns_beam = true;

        for frame_line in 0..scanlines {
            // Walk the beam 0..scanlines, the same way `run_frame_fast` does.
            // Free-running it from wherever the previous frame left off (the
            // phase `step()` produces when it owns the beam) puts the frame
            // boundary in the middle of VBlank, and a game whose main loop is
            // driven by the vertical interrupt then re-enters its handler
            // without ever leaving it: Dynamite Headdy freezes at $0118DE on
            // frame 44, with the debug tier and the fast tier having agreed on
            // every frame before it. Both runners present the same frame
            // to the program, which is also what makes a screenshot taken
            // here match what the player sees.
            let scanline = frame_line as u16;
            self.bus.vdp_mut().scanline(scanline);

            // Reset step master clock accumulator each line. Combined with
            // `frame_runner_owns_beam`, this keeps beam ownership exclusive to
            // this loop (step()'s scanline advance is for CLI single-step only).
            self.step_master_clocks_accumulated = 0;
            // Exact per-line 68K budget; carries the 4/7-cycle remainder.
            let cycles_per_scanline = self.next_scanline_cycles();

            // Check for VBLANK interrupt (VINT) when entering VBLANK
            let mut ev_vblank = false;
            let mut ev_vint = false;
            let mut ev_hint = false;
            if self.bus.vdp().should_trigger_vint() {
                ev_vblank = true;
                let vint_enabled = (self.bus.vdp().registers()[1] & 0x20) != 0;
                if vint_enabled && !self.bus.vdp().vint_request_delayed() {
                    self.cpu.request_interrupt(6); // VBLANK = IRQ level 6
                    ev_vint = true;
                }
                // Z80: VBLANK also triggers Z80 maskable IRQ
                self.z80_irq_pending = true;
                self.bus.vdp_mut().ack_vint();
            }

            // Clear Z80 IRQ when no longer in VBLANK (level-triggered)
            if !self.bus.vdp().in_vblank() {
                self.z80_irq_pending = false;
            }

            // Check for HBLANK interrupt (HINT)
            if self.bus.vdp().should_trigger_hint() {
                self.cpu.request_interrupt(4); // HBLANK = IRQ level 4
                self.bus.vdp_mut().ack_hint();
                ev_hint = true;
            }

            // Refresh VINT level from VDP state (level-triggered)
            self.refresh_vdp_interrupt_level();

            // Process pending interrupts (cycles count toward scanline budget)
            let pending_level = self.cpu.pending_irq_level;
            let irq_cycles = self.cpu.process_pending_interrupts(&mut self.bus);
            if irq_cycles > 0 {
                self.handle_interrupt_ack(pending_level);
                self.master_cycles += irq_cycles as u64;
            }

            // Raster/interrupt event breakpoints. Checked after the interrupt is
            // taken (so a VInt break lands with PC at the handler entry) and
            // costs nothing unless an event/watch breakpoint is armed.
            if self.check_frame_events(scanline, ev_vblank, ev_vint, ev_hint) {
                self.debugger.set_execution_mode(ExecutionMode::Paused);
                self.update_debugger_state();
                self.update_cached_framebuffer();
                self.frame_runner_owns_beam = false;
                return;
            }

            // Interleaved 68K + Z80 + YM2612 execution (same approach as run_frame_fast)
            let has_rom = self.bus.cartridge().is_some();
            let z80_tracing = self.debugger.is_z80_tracing_enabled();
            let z80_scanline = self.bus.vdp().timing().scanline;
            let psg_target = self.region.master_clocks_per_scanline() / 15;

            // Drain up to one line of pending 68k-bus DMA stall (freezes the CPU).
            let dma_stall = self.drain_dma_stall_for_line();
            let mut cycles_executed = irq_cycles + m68k_overshoot + dma_stall;
            let mut z80_target = 0u32;
            let mut z80_credit_numer = 0u32;
            let mut z80_cycles_executed = 0u32;
            let mut ym_cycles_clocked = 0u32;
            let mut z80_irq = self.z80_irq_pending;
            let mut early_exit = false;
            // When focused on Z80, step_z80() handles Z80 execution and audio
            // clocking internally; skip the interleaving to avoid double-stepping.
            let z80_focused = self.focused_cpu == 1;

            while has_rom && cycles_executed < cycles_per_scanline {
                let exec_mode = self.debugger.get_execution_mode();

                let step_cycles = match exec_mode {
                    ExecutionMode::Running => {
                        let step_result = self.step();
                        self.bus.vdp_mut().set_hblank_approx(
                            cycles_executed + step_result.cycles,
                            cycles_per_scanline,
                        );
                        if step_result.breakpoint_hit {
                            self.debugger.set_execution_mode(ExecutionMode::Paused);
                            self.z80_irq_pending = z80_irq;
                            self.update_cached_framebuffer();
                            self.frame_runner_owns_beam = false;
                            return;
                        }
                        step_result.cycles
                    }
                    ExecutionMode::Paused => {
                        self.z80_irq_pending = z80_irq;
                        self.update_cached_framebuffer();
                        self.frame_runner_owns_beam = false;
                        return;
                    }
                    ExecutionMode::StepInto | ExecutionMode::StepOver | ExecutionMode::StepOut => {
                        let step_result = self.step();
                        self.bus.vdp_mut().set_hblank_approx(
                            cycles_executed + step_result.cycles,
                            cycles_per_scanline,
                        );
                        self.debugger.set_execution_mode(ExecutionMode::Paused);
                        early_exit = true;
                        step_result.cycles
                    }
                };

                cycles_executed += step_cycles;

                // Handle Z80 reset-release transition per-instruction
                if self.bus.take_z80_reset_release() {
                    self.z80.pc = 0x0000;
                    self.z80.sp = 0xFFFF;
                    self.z80.iff1 = false;
                    self.z80.iff2 = false;
                    self.z80.im = 0;
                    self.z80.halted = false;
                    self.z80.ei = 0;
                }

                // Z80 interleaving only when 68K is the focused CPU.
                // When Z80 is focused, step_z80() already handles Z80+audio.
                if !z80_focused {
                    let ym_before_z80 = ym_cycles_clocked;

                    if !self.bus.is_z80_reset() && !self.bus.is_z80_bus_requested() {
                        z80_credit_numer += step_cycles * 7;
                        z80_target += z80_credit_numer / 15;
                        z80_credit_numer %= 15;
                    }

                    if z80_target > z80_cycles_executed + 3
                        && !self.bus.is_z80_reset()
                        && !self.bus.is_z80_bus_requested()
                    {
                        let mut z80_bus = self.bus.create_z80_bus();
                        while z80_cycles_executed < z80_target {
                            if z80_tracing {
                                let pc = self.z80.pc;
                                let mut raw_bytes = [0u8; 4];
                                for i in 0..4u16 {
                                    raw_bytes[i as usize] = z80_bus.read(pc.wrapping_add(i) as u32);
                                }
                                self.debugger.record_z80_trace(
                                    &self.z80,
                                    0,
                                    z80_scanline,
                                    raw_bytes,
                                );
                            }
                            let (processed, irq_cycles) =
                                self.z80.check_interrupts(&mut z80_bus, z80_irq, false);
                            if processed {
                                z80_irq = false;
                                z80_cycles_executed += irq_cycles;
                                let ym_c = (irq_cycles * 15 / 7)
                                    .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                                z80_bus.clock_ym2612(ym_c);
                                ym_cycles_clocked += ym_c;
                            }
                            self.debugger.record_coverage_z80(self.z80.pc);
                            let zc = self.z80.step(&mut z80_bus);
                            z80_cycles_executed += zc;
                            let ym_c = (zc * 15 / 7)
                                .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                            z80_bus.clock_ym2612(ym_c);
                            ym_cycles_clocked += ym_c;
                        }
                    }

                    // Ensure YM2612 is clocked by at least the 68K instruction's cycle count
                    let ym_target = ym_before_z80 + step_cycles;
                    if ym_cycles_clocked < ym_target {
                        let ym_remaining = (ym_target - ym_cycles_clocked)
                            .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                        if ym_remaining > 0 {
                            self.bus.ym2612_mut().clock(ym_remaining);
                            ym_cycles_clocked += ym_remaining;
                        }
                    }
                }

                if early_exit {
                    self.z80_irq_pending = z80_irq;
                    self.update_cached_framebuffer();
                    self.frame_runner_owns_beam = false;
                    return;
                }
            }

            // Top up Z80 to the scanline's proportional target
            if !z80_focused
                && !self.bus.is_z80_reset()
                && !self.bus.is_z80_bus_requested()
                && has_rom
            {
                let z80_max = psg_target;
                if z80_cycles_executed < z80_max {
                    let mut z80_bus = self.bus.create_z80_bus();
                    while z80_cycles_executed < z80_max {
                        if z80_tracing {
                            let pc = self.z80.pc;
                            let mut raw_bytes = [0u8; 4];
                            for i in 0..4u16 {
                                raw_bytes[i as usize] = z80_bus.read(pc.wrapping_add(i) as u32);
                            }
                            self.debugger
                                .record_z80_trace(&self.z80, 0, z80_scanline, raw_bytes);
                        }
                        let (processed, irq_cycles) =
                            self.z80.check_interrupts(&mut z80_bus, z80_irq, false);
                        if processed {
                            z80_irq = false;
                            z80_cycles_executed += irq_cycles;
                            let ym_c = (irq_cycles * 15 / 7)
                                .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                            z80_bus.clock_ym2612(ym_c);
                            ym_cycles_clocked += ym_c;
                        }
                        self.debugger.record_coverage_z80(self.z80.pc);
                        let zc = self.z80.step(&mut z80_bus);
                        z80_cycles_executed += zc;
                        let ym_c = (zc * 15 / 7)
                            .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                        z80_bus.clock_ym2612(ym_c);
                        ym_cycles_clocked += ym_c;
                    }
                }
            }

            self.z80_irq_pending = z80_irq;

            m68k_overshoot = cycles_executed.saturating_sub(cycles_per_scanline);

            // Top up YM2612 for remaining scanline cycles
            if ym_cycles_clocked < cycles_per_scanline {
                self.bus
                    .ym2612_mut()
                    .clock(cycles_per_scanline - ym_cycles_clocked);
            }

            // Clock PSG exactly psg_target Z80 cycles per scanline (always)
            self.bus.psg_mut().clock(psg_target);

            // I/O clocking
            // I/O ports run at 68K clock for 6-button timeout tracking (1.5ms = ~11,505 cycles)
            self.bus.io_mut().clock(cycles_per_scanline);
        }

        // Carry the final scanline's instruction overshoot into the next frame.
        self.frame_cycle_overshoot = m68k_overshoot;

        // Advance cartridge coprocessors to end-of-frame (SSP1601 seam).
        self.catch_up_cartridge();

        // Generate audio and mix into output buffer
        self.generate_and_mix_audio(true);

        self.frame_count += 1;
        self.frame_runner_owns_beam = false;

        // Update cached framebuffer from VDP
        self.update_cached_framebuffer();
    }

    fn run_until_vblank(&mut self) {
        // Runs one whole frame rather than stopping at the VBlank.
        self.run_frame();
    }

    fn load_rom(&mut self, data: &[u8]) -> Result<(), String> {
        use crate::cartridge::composable::{build_genesis_cartridge, build_with_manifest};

        // Build cartridge from a staged manifest override when present, else
        // via heuristic detection (auto-detects mapper/chip type, SVP before
        // the mapper match). The override's `preset` / `sram_size` / chips
        // drive the installed plan; hand-edited overrides need no SHA-256
        // gate here (the manifest is trusted metadata).
        let cartridge = match &self.cartridge_manifest_override {
            Some(manifest) => build_with_manifest(data.to_vec(), manifest)?,
            None => build_genesis_cartridge(data.to_vec())?,
        };

        // Remember the raw bytes so `detected_manifest` can re-detect the
        // live game's mapping for the editor.
        self.last_rom = data.to_vec();

        // Print ROM info (debug)
        let info = cartridge.info();
        // A cartridge sold only in PAL markets ran on a 50 Hz console and
        // its game logic (music tempo, timers, physics) assumes it; running
        // it at 60 Hz plays it 20 % fast. Multi-region and worldwide
        // releases stay NTSC: the 60 Hz build is what they shipped for
        // outside Europe. Applied after the reset below; an explicit region
        // set later (the netplay handshake) still wins, and `power_on_reset`
        // re-applies that override on top of this load.
        let header_region = if !info.regions.is_empty()
            && info
                .regions
                .iter()
                .all(|r| matches!(r, retro_core::cartridge::Region::Europe))
        {
            VideoRegion::Pal
        } else {
            VideoRegion::Ntsc
        };
        eprintln!("Loaded ROM: {}", info.title);
        eprintln!("  System: {:?}", info.system);
        eprintln!("  ROM Size: {} bytes", info.rom_size);
        if info.sram_size > 0 {
            eprintln!("  SRAM: {} bytes ({:?})", info.sram_size, info.sram_type);
        }
        if let Some(ref mapper_info) = info.mapper_info {
            eprintln!("  Mapper: {:?}", mapper_info.mapper_id);
        }
        if let Some(ref chip_info) = info.expansion_chip_info {
            eprintln!("  Expansion Chip: {:?}", chip_info.chip_type);
        }

        // Verify checksum (warn if mismatch)
        if let (Some(header_ck), Some(calc_ck)) = (info.header_checksum, info.calculated_checksum) {
            if header_ck != calc_ck {
                eprintln!("WARNING: ROM checksum mismatch!");
                eprintln!("  Expected: 0x{:04X}", header_ck);
                eprintln!("  Calculated: 0x{:04X}", calc_ck);
            }
        }

        // Decide whether to wipe the per-CPU coverage bitmaps. They
        // describe *which addresses are real instruction starts* for
        // the loaded ROM, so loading a different cartridge invalidates
        // every entry. Hot-reload of the same bytes (Bass build with
        // no-op output, or external load_rom of identical data) keeps
        // the bitmap so the disassembly window stays populated.
        let new_hash = info.rom_hash.clone();
        let same_rom = self
            .get_cartridge_hash()
            .map(|h| h == new_hash)
            .unwrap_or(false);

        if !same_rom {
            self.debugger.clear_coverage(0);
            self.debugger.clear_coverage(1);

            // Swapping in a *different* cartridge is a power cycle, not a
            // press of the reset button: on real hardware you eject the cart
            // with the console off. `reset()` alone would leave the outgoing
            // game's work RAM, Z80 RAM, VDP register file (plane/sprite-table
            // base addresses!), I/O ports and bus latches in place, and the
            // incoming game would boot on top of them, which is exactly how a
            // title that does not rewrite every VDP register comes up with
            // corrupt graphics after another game has run. Rebuild the
            // volatile hardware first; the cartridge is re-plugged with its
            // SRAM intact.
            self.rebuild_volatile_hardware();
        }

        // Load cartridge into bus
        self.bus.set_cartridge(cartridge);

        // Reset system with new ROM
        self.reset();
        self.set_region(header_region);

        // The ROM's I/O-support field (header $190-$19F) names a "6" when
        // the game expects 6-button pads. Contra Hard Corps
        // advertises `J6` and its attract demo forks if it only ever sees
        // 3-button TH replies.
        self.connect_controllers_for_rom(data);

        Ok(())
    }

    fn get_rom_info(&self) -> Option<RomInfo> {
        self.bus.cartridge().map(|cart| {
            let info = cart.info();
            let confidence = if info.title.is_empty() {
                "none"
            } else if info.title.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
                "high"
            } else {
                "low"
            };
            RomInfo {
                title: info.title.clone(),
                region: info
                    .regions
                    .iter()
                    .map(|r| format!("{:?}", r))
                    .collect::<Vec<_>>()
                    .join(", "),
                size: info.rom_size,
                checksum: info.header_checksum.unwrap_or(0) as u32,
                mapper: info
                    .mapper_info
                    .as_ref()
                    .map(|m| format!("{:?}", m.mapper_id)),
                title_confidence: confidence.to_string(),
            }
        })
    }

    fn get_framebuffer(&self) -> &Framebuffer {
        &self.cached_framebuffer
    }

    fn get_display_info(&self) -> &DisplayInfo {
        &self.system_info.display
    }

    fn get_audio_samples(&mut self) -> &[f32] {
        // Drain the ring into the scratch buffer so we can satisfy
        // the trait's `&[f32]` contract without per-call allocation.
        // Hosts that can take an explicit count should call
        // `drain_audio_into(&mut [f32]) -> usize` on the concrete
        // type to skip the resize.
        let n = self.audio_ring.available();
        self.pending_audio_drain.clear();
        self.pending_audio_drain.resize(n, 0.0);
        self.audio_ring.drain_into(&mut self.pending_audio_drain);
        &self.pending_audio_drain
    }

    fn get_audio_info(&self) -> &AudioInfo {
        &self.system_info.audio
    }

    fn set_audio_enabled(&mut self, enabled: bool) {
        self.audio_enabled = enabled;
        // The chips render incrementally (before each register write); gate
        // that catch-up synthesis too, so disabling audio saves its cost.
        self.bus.ym2612_mut().set_rendering_enabled(enabled);
        self.bus.psg_mut().set_rendering_enabled(enabled);
        if !enabled {
            self.audio_ring.clear();
        }
    }

    fn is_audio_enabled(&self) -> bool {
        self.audio_enabled
    }

    fn set_input(&mut self, player: u8, input: InputState) {
        // Route the bitmask through `InputManager` as a "direct override"
        // that OR-merges with whatever provider (keyboard / gamepad) is
        // assigned to the port. This is the path the JS frontend uses for
        // virtual-input sources like the mobile touch overlay: they get
        // to inject buttons without having to swap the keyboard out of
        // port 0 or auto-claim port 1 (which would silently bump them to
        // player 2).
        //
        // Frontends that don't want this layered behaviour can keep using
        // `keyDown` / `gamepadButton` exclusively; `set_input(_, 0)` clears
        // the override so the port reverts to provider-only state.
        self.input_manager
            .set_direct_override(player as usize, input.buttons);
    }

    fn get_input_info(&self) -> &InputInfo {
        &self.system_info.input
    }

    fn save_state(&self) -> Vec<u8> {
        use crate::system::savestate::*;
        use retro_core::savestate::{
            serialize_compressed, SaveStateEnvelope, RETRO_SAVESTATE_ENVELOPE_VERSION,
        };

        let (game_title, rom_hash) = self
            .bus
            .cartridge()
            .map(|c| (c.info().title.clone(), c.info().rom_hash.clone()))
            .unwrap_or_default();

        let state = self.build_save_state(SaveStateMetadata::now(game_title, rom_hash.clone(), 0));

        // Wrap the SG1-specific snapshot in the cross-core envelope.
        // `created_at` mirrors chrono's RFC3339 timestamp in
        // `state.metadata.timestamp` but encoded as a plain unix
        // seconds counter for tooling that doesn't parse RFC3339.
        let envelope = SaveStateEnvelope {
            envelope_version: RETRO_SAVESTATE_ENVELOPE_VERSION,
            core_id: "sg1".to_string(),
            core_version: SAVE_STATE_VERSION,
            rom_hash,
            created_at: chrono::Utc::now().timestamp().max(0) as u64,
            payload: state,
        };

        serialize_compressed(&envelope, 3).unwrap_or_default()
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        self.load_state_with_rom_check(data, retro_core::savestate::RomCheck::SameRom)
            .map_err(|e| e.to_string())
    }

    fn get_save_ram(&self) -> Option<&[u8]> {
        self.bus
            .cartridge()
            .map(|c| c.sram())
            .filter(|s| !s.is_empty())
    }

    fn set_save_ram(&mut self, data: &[u8]) {
        if let Some(cart) = self.bus.cartridge_mut() {
            cart.restore_sram(data);
        }
    }

    fn debugger(&mut self) -> &mut dyn DebuggerCore {
        &mut self.debugger
    }

    fn read_memory_debug(&mut self, addr: u32, size: u32) -> Vec<u8> {
        Self::read_memory_debug(self, addr, size)
    }

    fn disassemble_at(&mut self, addr: u32, count: u32) -> Vec<Instruction> {
        Self::disassemble_at(self, addr, count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_genesis_creation() {
        let genesis = GenesisEmulator::new();
        let info = genesis.system_info();

        assert_eq!(info.name, "Sega Genesis");
        assert_eq!(info.cpus.len(), 2);
        assert_eq!(info.cpus[0].architecture, CpuArchitecture::M68000);
        assert_eq!(info.cpus[1].architecture, CpuArchitecture::Z80);
        assert_eq!(genesis.frame_count(), 0);
    }

    #[test]
    fn test_display_info() {
        let genesis = GenesisEmulator::new();
        let display = genesis.get_display_info();

        assert_eq!(display.width, 320);
        assert_eq!(display.height, 224);
    }

    #[test]
    fn test_framebuffer() {
        let genesis = GenesisEmulator::new();
        let fb = genesis.get_framebuffer();

        assert_eq!(fb.width, 320);
        assert_eq!(fb.height, 224);
        assert_eq!(fb.data.len(), 320 * 224 * 4);
    }

    #[test]
    fn repaint_held_frame_pushes_vdp_state_into_cached_framebuffer() {
        // A paused debugger edit (layer toggle, VRAM/CRAM write)
        // must reach the host. The browser reads get_framebuffer(), the
        // cached copy, so repaint must re-render the VDP *and* re-sync that
        // cache. Drive a distinct backdrop through the public VDP ports, then
        // confirm the cached framebuffer paints it and tracks a later change.
        // (Display ON + empty VRAM => both planes are transparent and the whole
        // screen falls through to the backdrop colour.)
        use crate::vdp::Vdp;

        fn set_reg(vdp: &mut Vdp, reg: u16, val: u16) {
            vdp.write_word_port(0x04, 0x8000 | (reg << 8) | (val & 0xFF));
        }
        fn write_cram(vdp: &mut Vdp, entry: u16, color: u16) {
            // CRAM-write code (CD=0b0011): first ctrl word top bits = 0b11.
            vdp.write_word_port(0x04, 0xC000 | ((entry * 2) & 0x3FFF));
            vdp.write_word_port(0x04, 0x0000);
            vdp.write_word_port(0x00, color);
        }
        fn top_left(genesis: &GenesisEmulator) -> (u8, u8, u8) {
            let d = &genesis.get_framebuffer().data;
            (d[0], d[1], d[2])
        }

        let mut genesis = GenesisEmulator::new();
        {
            let vdp = genesis.vdp_mut();
            set_reg(vdp, 1, 0x44); // display on, mode 5
            write_cram(vdp, 1, 0x0E00); // CRAM[1] = bright blue
            set_reg(vdp, 7, 0x01); // backdrop -> CRAM entry 1
        }

        genesis.repaint_held_frame();
        let green = top_left(&genesis);
        assert_ne!(
            green,
            (0, 0, 0),
            "repaint must composite the backdrop into the cached framebuffer"
        );

        // Change the backdrop colour while paused and repaint again: the
        // cached frame the host reads must track the new VDP state.
        write_cram(genesis.vdp_mut(), 1, 0x000E); // CRAM[1] = bright red
        genesis.repaint_held_frame();
        assert_ne!(
            top_left(&genesis),
            green,
            "a paused VDP change must reach the cached framebuffer after repaint"
        );
    }

    /// Helper to create a valid Genesis ROM for testing
    fn create_test_rom() -> Vec<u8> {
        // Create 128KB ROM (enough for multiple frames)
        let mut rom = Vec::with_capacity(131072);
        for _ in 0..65536 {
            rom.push(0x4E);
            rom.push(0x71);
        }

        // Reset vectors
        rom[0] = 0x00; // SSP high
        rom[1] = 0xFF;
        rom[2] = 0x00;
        rom[3] = 0x00;
        rom[4] = 0x00; // PC
        rom[5] = 0x00;
        rom[6] = 0x04;
        rom[7] = 0x00;

        // Genesis header
        rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");
        rom[0x120..0x138].copy_from_slice(b"TEST ROM                ");
        // ROM $000000-$01FFFF and work RAM $FF0000-$FFFFFF, as a real
        // header gives them. Left as NOP bytes, the RAM field reads as an
        // SRAM range of $4E000001 bytes, which the header parser cuts to its
        // 4 MB limit: every emulator a test builds would allocate 4 MB for it.
        rom[0x1A0..0x1B0].copy_from_slice(&[
            0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0xFF, 0xFF, 0x00, 0xFF, 0x00, 0x00, 0x00, 0xFF,
            0xFF, 0xFF,
        ]);

        // At 0x400, put NOPs followed by infinite loop at the end
        // This ensures execution doesn't go off the rails
        let loop_addr = 0x1FFF0; // Near end of ROM
        rom[loop_addr] = 0x60; // BRA.S
        rom[loop_addr + 1] = 0xFE; // -2 (infinite loop)

        rom
    }

    #[test]
    fn test_reset() {
        let mut genesis = GenesisEmulator::new();
        let rom = create_test_rom();
        genesis.load_rom(&rom).unwrap();

        assert_eq!(genesis.cpu().pc, 0x00000400);
        assert_eq!(genesis.frame_count(), 0);
    }

    #[test]
    fn a_pause_reports_where_the_fast_loop_stopped() {
        use retro_core::DebuggerCore;

        let mut genesis = GenesisEmulator::new();
        genesis.load_rom(&create_test_rom()).unwrap();
        genesis.set_execution_mode(ExecutionMode::Running);
        for _ in 0..3 {
            genesis.run_frame_fast();
        }
        // The fast loop writes no CPU state: the debugger still holds the one
        // the reset wrote.
        assert_eq!(genesis.debugger().get_cpu_states()[0].pc, 0x400);

        genesis.set_execution_mode(ExecutionMode::Paused);

        let pc = genesis.cpu().pc;
        assert_ne!(pc, 0x400);
        assert_eq!(genesis.debugger().get_cpu_states()[0].pc, pc);
    }

    #[test]
    fn power_on_reset_zeroes_volatile_ram() {
        let mut genesis = GenesisEmulator::new();
        genesis.load_rom(&create_test_rom()).unwrap();

        // Dirty the volatile RAM the way a running game leaves it.
        genesis.bus.memory_mut().work_ram.as_mut_slice().fill(0xAB);
        genesis.bus.memory_mut().z80_ram.as_mut_slice().fill(0xCD);

        genesis.power_on_reset();

        assert!(
            genesis
                .bus
                .memory()
                .work_ram
                .as_slice()
                .iter()
                .all(|&b| b == 0),
            "power-on reset must zero the 68K work RAM"
        );
        assert!(
            genesis
                .bus
                .memory()
                .z80_ram
                .as_slice()
                .iter()
                .all(|&b| b == 0),
            "power-on reset must zero the Z80 RAM"
        );
        assert_eq!(genesis.frame_count(), 0);
        assert_eq!(
            genesis.cpu().pc,
            0x00000400,
            "cold reset still boots the ROM vector"
        );
    }

    #[test]
    fn soft_reset_preserves_usp_but_power_on_reset_clears_it() {
        // A soft reset re-boots the 68000 (PC/SP/SR from the vector, GP registers
        // cleared) but, faithful to hardware, does NOT clear the user stack
        // pointer, so a host that has been playing and a fresh guest diverge on
        // `cpu68k` and their netplay start commitments differ. A power-on reset
        // rebuilds the machine, clearing it.
        let mut genesis = GenesisEmulator::new();
        genesis.load_rom(&create_test_rom()).unwrap();
        genesis.cpu_mut().usp = 0x00CA_FE00;

        // Soft reset: USP survives (one way two netplay peers diverge).
        EmulatorCore::reset(&mut genesis);
        assert_eq!(
            genesis.cpu().usp,
            0x00CA_FE00,
            "a soft reset must NOT clear the 68K user stack pointer (hardware-faithful)"
        );

        // Power-on reset: the machine is rebuilt, so USP is cleared.
        genesis.cpu_mut().usp = 0x00CA_FE00;
        genesis.power_on_reset();
        assert_eq!(
            genesis.cpu().usp,
            0,
            "power-on reset must clear the 68K user stack pointer"
        );
    }

    #[test]
    fn power_on_reset_converges_cpu_and_ram_across_divergent_history() {
        // The netplay committed-start premise: a host that has been running (its
        // 68K state + RAM dirtied) and a freshly-joined guest must reach a byte-identical
        // frame-0 once both cold-reset. Compares the (tiny) 68K savestate + RAM
        // directly rather than the full `state_hash`; the SNES convergence test
        // exercises the full `state_hash` path.
        let rom = create_test_rom();
        let cpu_state = |g: &GenesisEmulator| bincode::serialize(&g.cpu().to_savestate()).unwrap();

        let mut host = GenesisEmulator::new();
        host.load_rom(&rom).unwrap();
        // Dirty the 68K field a soft reset preserves (USP) plus the volatile RAM.
        host.cpu_mut().usp = 0x00CA_FE00;
        host.bus.memory_mut().work_ram.as_mut_slice().fill(0x5A);
        host.bus.memory_mut().z80_ram.as_mut_slice().fill(0xA5);

        let mut guest = GenesisEmulator::new();
        guest.load_rom(&rom).unwrap();

        // Sanity: the two genuinely diverge before the cold reset.
        assert_ne!(
            cpu_state(&host),
            cpu_state(&guest),
            "divergent 68K state should differ before the cold reset"
        );

        host.power_on_reset();
        guest.power_on_reset();

        assert_eq!(
            cpu_state(&host),
            cpu_state(&guest),
            "cold reset converges the 68K state"
        );
        assert_eq!(
            host.bus.memory().work_ram.as_slice(),
            guest.bus.memory().work_ram.as_slice(),
            "cold reset converges the work RAM"
        );
        assert_eq!(
            host.bus.memory().z80_ram.as_slice(),
            guest.bus.memory().z80_ram.as_slice(),
            "cold reset converges the Z80 RAM"
        );
    }

    #[test]
    fn manifest_override_changes_installed_plan() {
        let mut genesis = GenesisEmulator::new();
        let rom = create_test_rom();

        // Pure detection: a small plain cartridge.
        genesis.load_rom(&rom).unwrap();
        assert_eq!(genesis.bus.cartridge().unwrap().map_plan().name, "plain");
        assert!(genesis
            .bus
            .cartridge()
            .unwrap()
            .coprocessor_label()
            .is_none());
        // The live ROM is remembered so the editor can re-detect it.
        assert_eq!(
            genesis.detected_manifest(genesis.last_rom()).mapping.preset,
            "plain"
        );

        // Override: force the SEGA mapper, then reload.
        let mut over = genesis.detected_manifest(&rom);
        over.mapping.preset = "sega".to_string();
        genesis.set_cartridge_manifest_override(Some(over));
        genesis.load_rom(&rom).unwrap();
        assert_eq!(genesis.bus.cartridge().unwrap().map_plan().name, "sega");
        assert_eq!(
            genesis.bus.cartridge().unwrap().coprocessor_label(),
            Some("SEGA mapper")
        );

        // Clearing the override reverts to pure detection.
        genesis.set_cartridge_manifest_override(None);
        genesis.load_rom(&rom).unwrap();
        assert_eq!(genesis.bus.cartridge().unwrap().map_plan().name, "plain");
    }

    /// End-to-end SVP memory-controller test through the real 68000 bus: the
    /// "SV" tag at ROM 0x1C8 builds a composable SVP cartridge, and the DRAM
    /// window, the two cell-arrange views, and the 0xA15000 mailbox all
    /// dispatch to the SVP coprocessor. A system savestate round-trips the
    /// SVP's shared DRAM and mailbox.
    #[test]
    fn svp_cartridge_maps_windows_and_savestate_round_trips() {
        let mut genesis = GenesisEmulator::new();
        let mut rom = create_test_rom();
        // The SVP marker in the header.
        rom[0x1C8] = b'S';
        rom[0x1C9] = b'V';
        genesis.load_rom(&rom).unwrap();

        // Detected + installed as a composable SVP cartridge.
        assert_eq!(genesis.bus.cartridge().unwrap().map_plan().name, "svp");
        assert_eq!(
            genesis.bus.cartridge().unwrap().coprocessor_label(),
            Some("SVP")
        );

        // DRAM window (banks 0x30/0x31) round-trips a word.
        genesis.bus.write_word(0x300000, 0x1234);
        assert_eq!(genesis.bus.read_word(0x300000), 0x1234);

        // Cell-arrange view 1 (0x390000) reads the same low DRAM word (the
        // permutation of address 0 is 0).
        assert_eq!(genesis.bus.read_word(0x390000), 0x1234);
        // Cell-arrange view 2 (0x3A0000) likewise.
        assert_eq!(genesis.bus.read_word(0x3A0000), 0x1234);

        // Mailbox: a 68000 byte write to 0xA15000/0xA15001 sets XST; reading
        // it back returns the high then low byte (the handshake round-trips).
        genesis.bus.write(0xA15000, 0xBE);
        genesis.bus.write(0xA15001, 0xEF);
        assert_eq!(genesis.bus.read(0xA15000), 0xBE);
        assert_eq!(genesis.bus.read(0xA15001), 0xEF);
        // Reading the PM0 status register (0xA15004) is a defined access.
        let _ = genesis.bus.read(0xA15004);

        // The SVP coprocessor was exercised by all of the above.
        assert!(genesis.bus.cartridge().unwrap().coprocessor_accesses() > 0);

        // Savestate round-trips the SVP DRAM + mailbox through the typed
        // multi-chip bridge.
        let snapshot = genesis.save_state();
        assert!(!snapshot.is_empty());
        // Clobber DRAM + mailbox after the snapshot.
        genesis.bus.write_word(0x300000, 0x5A5A);
        genesis.bus.write(0xA15000, 0x00);
        genesis.bus.write(0xA15001, 0x00);
        assert_eq!(genesis.bus.read_word(0x300000), 0x5A5A);

        genesis.load_state(&snapshot).unwrap();
        assert_eq!(genesis.bus.read_word(0x300000), 0x1234, "DRAM restored");
        assert_eq!(genesis.bus.read(0xA15000), 0xBE, "XST high restored");
        assert_eq!(genesis.bus.read(0xA15001), 0xEF, "XST low restored");
    }

    #[test]
    fn test_step() {
        let mut genesis = GenesisEmulator::new();
        let rom = create_test_rom();
        genesis.load_rom(&rom).unwrap();

        let initial_pc = genesis.cpu().pc;
        let result = genesis.step();

        assert!(result.cycles > 0);
        assert!(!result.breakpoint_hit);
        assert_ne!(genesis.cpu().pc, initial_pc); // PC should have advanced
    }

    /// Build a test ROM whose first instruction writes a word to work RAM,
    /// then spins. Reset PC is 0x400 (see `create_test_rom`).
    ///   MOVE.W #$1234, ($00FF0000).L   ; 33 FC 12 34 00 FF 00 00
    ///   BRA.S  *                       ; 60 FE
    fn create_ram_write_rom() -> Vec<u8> {
        let mut rom = create_test_rom();
        let prog = [0x33u8, 0xFC, 0x12, 0x34, 0x00, 0xFF, 0x00, 0x00, 0x60, 0xFE];
        rom[0x400..0x400 + prog.len()].copy_from_slice(&prog);
        rom
    }

    /// `MOVE.W ($00000500).L, D0` then `BRA.S *`: one read of a ROM table.
    const ROM_READ_PROGRAM: [u8; 8] = [0x30, 0x39, 0x00, 0x00, 0x05, 0x00, 0x60, 0xFE];

    /// A test ROM running [`ROM_READ_PROGRAM`] from the reset PC (0x400),
    /// with the word `$ABCD` at 0x500.
    fn create_rom_read_rom() -> Vec<u8> {
        let mut rom = create_test_rom();
        rom[0x400..0x408].copy_from_slice(&ROM_READ_PROGRAM);
        rom[0x500] = 0xAB;
        rom[0x501] = 0xCD;
        rom
    }

    /// What the `MOVE.W` of [`ROM_READ_PROGRAM`] at `pc` does on the bus:
    /// its opcode and two extension words, then the table read.
    fn rom_read_accesses(pc: u32) -> Vec<(u32, u8, bool, u16)> {
        vec![
            (pc, 2, false, 0x3039),
            (pc + 2, 2, false, 0x0000),
            (pc + 4, 2, false, 0x0500),
            (0x500, 2, false, 0xABCD),
        ]
    }

    #[test]
    fn the_instruction_access_log_keeps_one_instruction_and_its_pc() {
        let mut genesis = GenesisEmulator::new();
        genesis.load_rom(&create_rom_read_rom()).unwrap();

        // Only the instruction's own accesses: not the step's look at the
        // opcode nor the disassembly it makes for the history.
        genesis.set_instruction_access_logging(true);
        genesis.step();
        let log = genesis.take_instruction_access_log();
        assert_eq!(log.pc, 0x400);
        assert_eq!(log.accesses, rom_read_accesses(0x400));
        assert_eq!(
            genesis.take_instruction_access_log(),
            InstructionAccessLog::default(),
            "taking the log empties it"
        );

        genesis.set_instruction_access_logging(false);
        genesis.step();
        assert_eq!(
            genesis.take_instruction_access_log(),
            InstructionAccessLog::default(),
            "nothing is kept while the log is off"
        );
    }

    #[test]
    fn the_instruction_access_log_leaves_out_the_interrupt_entered_before_it() {
        // Level-4 autovector -> a handler at 0x600 that makes the same read.
        let mut rom = create_rom_read_rom();
        rom[0x70..0x74].copy_from_slice(&[0x00, 0x00, 0x06, 0x00]);
        rom[0x600..0x608].copy_from_slice(&ROM_READ_PROGRAM);
        let mut genesis = GenesisEmulator::new();
        genesis.load_rom(&rom).unwrap();
        genesis.cpu_mut().sr = 0x2000; // supervisor, interrupt mask 0
        genesis.cpu_mut().request_interrupt(4);

        genesis.set_instruction_access_logging(true);
        genesis.step();
        let log = genesis.take_instruction_access_log();
        // The step entered the interrupt (stacking PC and SR, reading the
        // vector) and ran the handler's first instruction; only that
        // instruction's accesses, under its own address, are kept.
        assert_eq!(log.pc, 0x600);
        assert_eq!(log.accesses, rom_read_accesses(0x600));
    }

    /// Starts a DMA of 16 words from ROM 0x500 to VRAM 0x4000: eight
    /// `MOVE.W #imm, ($C00004).L` set registers 1, 15 and 19-23 and the
    /// command's first word, and the ninth writes its second word.
    fn create_dma_from_rom_rom() -> Vec<u8> {
        let mut rom = create_test_rom();
        let words: [u16; 9] = [
            0x8114, 0x8F02, 0x9310, 0x9400, 0x9580, 0x9602, 0x9700, 0x4000, 0x0081,
        ];
        let mut at = 0x400;
        for w in words {
            let [hi, lo] = w.to_be_bytes();
            rom[at..at + 8].copy_from_slice(&[0x33, 0xFC, hi, lo, 0x00, 0xC0, 0x00, 0x04]);
            at += 8;
        }
        rom[at..at + 2].copy_from_slice(&[0x60, 0xFE]);
        rom
    }

    #[test]
    fn the_instruction_access_log_keeps_the_dma_an_instruction_starts() {
        use crate::vdp::dma::DmaTarget;

        let mut genesis = GenesisEmulator::new();
        genesis.load_rom(&create_dma_from_rom_rom()).unwrap();
        genesis.set_instruction_access_logging(true);
        for _ in 0..8 {
            genesis.step();
            assert!(genesis.take_instruction_access_log().dmas.is_empty());
        }

        genesis.step();
        let log = genesis.take_instruction_access_log();
        assert_eq!(log.pc, 0x440);
        assert_eq!(
            log.dmas,
            vec![MemoryDma {
                source: 0x500,
                words: 16,
                target: DmaTarget::Vram,
                destination: 0x4000,
            }]
        );
        // The VDP read the source itself: it is not a 68000 read.
        assert!(log.accesses.iter().all(|&(addr, ..)| addr != 0x500));
    }

    fn watch_hits(genesis: &GenesisEmulator, id: BreakpointId) -> u32 {
        genesis
            .debugger_ref()
            .list_breakpoints()
            .iter()
            .find(|b| b.id == id)
            .map(|b| b.hit_count)
            .expect("breakpoint should still exist")
    }

    #[test]
    fn test_write_watchpoint_fires_on_ram_write() {
        use retro_core::DebuggerCore;

        let mut genesis = GenesisEmulator::new();
        genesis.load_rom(&create_ram_write_rom()).unwrap();

        // The 68K is CPU id 0; a write watch on the destination word must
        // trip when the MOVE writes there.
        let id = genesis.debugger().add_breakpoint(Breakpoint {
            address: 0x00FF0000,
            bp_type: BreakpointType::write(),
            cpu_id: Some(0),
            condition: None,
        });

        // First instruction at the reset PC is the MOVE.W.
        let result = genesis.step();

        assert!(
            result.breakpoint_hit,
            "write watchpoint should fire on the RAM write"
        );
        assert_eq!(watch_hits(&genesis, id), 1);
    }

    #[test]
    fn test_write_watchpoint_ignores_unrelated_address() {
        use retro_core::DebuggerCore;

        let mut genesis = GenesisEmulator::new();
        genesis.load_rom(&create_ram_write_rom()).unwrap();

        // Watch a word the program never touches.
        let id = genesis.debugger().add_breakpoint(Breakpoint {
            address: 0x00FF1000,
            bp_type: BreakpointType::write(),
            cpu_id: Some(0),
            condition: None,
        });

        let result = genesis.step();

        assert!(
            !result.breakpoint_hit,
            "watchpoint on an unwritten address must not fire"
        );
        assert_eq!(watch_hits(&genesis, id), 0);
    }

    #[test]
    fn test_read_watchpoint_does_not_fire_on_write() {
        use retro_core::DebuggerCore;

        let mut genesis = GenesisEmulator::new();
        genesis.load_rom(&create_ram_write_rom()).unwrap();

        // A read-only watch on the destination must stay silent when the
        // access is a write (direction is honored independently of address).
        let id = genesis.debugger().add_breakpoint(Breakpoint {
            address: 0x00FF0000,
            bp_type: BreakpointType::read(),
            cpu_id: Some(0),
            condition: None,
        });

        let result = genesis.step();

        assert!(
            !result.breakpoint_hit,
            "read watchpoint must not fire on a write access"
        );
        assert_eq!(watch_hits(&genesis, id), 0);
    }

    #[test]
    fn test_value_matched_write_watchpoint_fires_on_matching_byte() {
        use retro_core::types::MatchSpec;
        use retro_core::DebuggerCore;

        let mut genesis = GenesisEmulator::new();
        genesis.load_rom(&create_ram_write_rom()).unwrap();

        // MOVE.W #$1234 -> ($00FF0000): high byte 0x12 lands at 0x00FF0000.
        let id = genesis.debugger().add_breakpoint(Breakpoint {
            address: 0x00FF0000,
            bp_type: BreakpointType::write(),
            cpu_id: Some(0),
            condition: None,
        });
        genesis.debugger().set_breakpoint_match_spec(
            id,
            MatchSpec {
                value: Some(0x12),
                ..Default::default()
            },
        );

        let result = genesis.step();
        assert!(
            result.breakpoint_hit,
            "value watchpoint should fire when the written byte matches"
        );
        assert_eq!(watch_hits(&genesis, id), 1);
    }

    #[test]
    fn test_value_matched_write_watchpoint_ignores_other_values() {
        use retro_core::types::MatchSpec;
        use retro_core::DebuggerCore;

        let mut genesis = GenesisEmulator::new();
        genesis.load_rom(&create_ram_write_rom()).unwrap();

        // The byte written at 0x00FF0000 is 0x12, not 0x99.
        let id = genesis.debugger().add_breakpoint(Breakpoint {
            address: 0x00FF0000,
            bp_type: BreakpointType::write(),
            cpu_id: Some(0),
            condition: None,
        });
        genesis.debugger().set_breakpoint_match_spec(
            id,
            MatchSpec {
                value: Some(0x99),
                ..Default::default()
            },
        );

        let result = genesis.step();
        assert!(
            !result.breakpoint_hit,
            "value watchpoint must stay silent when the written byte differs"
        );
        assert_eq!(watch_hits(&genesis, id), 0);
    }

    #[test]
    fn test_value_matched_watchpoint_picks_low_byte_of_word() {
        use retro_core::types::MatchSpec;
        use retro_core::DebuggerCore;

        let mut genesis = GenesisEmulator::new();
        genesis.load_rom(&create_ram_write_rom()).unwrap();

        // Low byte 0x34 lands at 0x00FF0001 (big-endian word write).
        let id = genesis.debugger().add_breakpoint(Breakpoint {
            address: 0x00FF0001,
            bp_type: BreakpointType::write(),
            cpu_id: Some(0),
            condition: None,
        });
        genesis.debugger().set_breakpoint_match_spec(
            id,
            MatchSpec {
                value: Some(0x34),
                ..Default::default()
            },
        );

        let result = genesis.step();
        assert!(
            result.breakpoint_hit,
            "value watchpoint should match the low byte of a word write"
        );
        assert_eq!(watch_hits(&genesis, id), 1);
    }

    /// MOVE.L #$12345678, ($00FF0000).L ; BRA.S *: writes a full 32-bit
    /// value to work RAM so multi-byte watchpoints have something to
    /// assemble.
    fn create_ram_write_long_rom() -> Vec<u8> {
        let mut rom = create_test_rom();
        let prog = [
            0x23u8, 0xFC, 0x12, 0x34, 0x56, 0x78, 0x00, 0xFF, 0x00, 0x00, 0x60, 0xFE,
        ];
        rom[0x400..0x400 + prog.len()].copy_from_slice(&prog);
        rom
    }

    #[test]
    fn test_sized_32bit_watchpoint_fires_on_assembled_long() {
        use retro_core::types::MatchSpec;
        use retro_core::DebuggerCore;

        let mut genesis = GenesisEmulator::new();
        genesis.load_rom(&create_ram_write_long_rom()).unwrap();

        // A 32-bit watch at 0xFF0000 fires when the assembled big-endian
        // long at 0xFF0000..0xFF0003 equals the target, even though the
        // MOVE.L writes it as two separate word stores.
        let id = genesis.debugger().add_breakpoint(Breakpoint {
            address: 0x00FF0000,
            bp_type: BreakpointType::write(),
            cpu_id: Some(0),
            condition: None,
        });
        genesis.debugger().set_breakpoint_match_spec(
            id,
            MatchSpec {
                value: Some(0x12345678),
                value_size: 4,
                ..Default::default()
            },
        );

        let result = genesis.step();
        assert!(
            result.breakpoint_hit,
            "32-bit watch should fire when the assembled long matches"
        );
        // The four covered bytes are all written this instruction, but the
        // watch must count a single hit, not one per byte.
        assert_eq!(watch_hits(&genesis, id), 1);
    }

    #[test]
    fn test_sized_32bit_watchpoint_ignores_other_values() {
        use retro_core::types::MatchSpec;
        use retro_core::DebuggerCore;

        let mut genesis = GenesisEmulator::new();
        genesis.load_rom(&create_ram_write_long_rom()).unwrap();

        let id = genesis.debugger().add_breakpoint(Breakpoint {
            address: 0x00FF0000,
            bp_type: BreakpointType::write(),
            cpu_id: Some(0),
            condition: None,
        });
        genesis.debugger().set_breakpoint_match_spec(
            id,
            MatchSpec {
                value: Some(0xDEADBEEF),
                value_size: 4,
                ..Default::default()
            },
        );

        let result = genesis.step();
        assert!(
            !result.breakpoint_hit,
            "32-bit watch must stay silent when the assembled long differs"
        );
        assert_eq!(watch_hits(&genesis, id), 0);
    }

    #[test]
    fn test_sized_16bit_watchpoint_fires_via_consecutive_byte() {
        use retro_core::types::MatchSpec;
        use retro_core::DebuggerCore;

        let mut genesis = GenesisEmulator::new();
        // MOVE.W #$1234 -> ($00FF0000): bytes 0x12 0x34 at 0xFF0000-1.
        genesis.load_rom(&create_ram_write_rom()).unwrap();

        // A 16-bit watch keyed at 0xFF0000 with value 0x1234 fires even
        // when only the low byte 0xFF0001 is named as the base? No, the
        // base is 0xFF0000 and width 2 covers 0xFF0000..0xFF0001.
        let id = genesis.debugger().add_breakpoint(Breakpoint {
            address: 0x00FF0000,
            bp_type: BreakpointType::write(),
            cpu_id: Some(0),
            condition: None,
        });
        genesis.debugger().set_breakpoint_match_spec(
            id,
            MatchSpec {
                value: Some(0x1234),
                value_size: 2,
                ..Default::default()
            },
        );

        let result = genesis.step();
        assert!(
            result.breakpoint_hit,
            "16-bit watch should fire when the assembled word matches"
        );
        assert_eq!(watch_hits(&genesis, id), 1);
    }

    #[test]
    fn test_conditional_execute_breakpoint_fires_when_expression_true() {
        use retro_core::types::MatchSpec;
        use retro_core::DebuggerCore;

        let mut genesis = GenesisEmulator::new();
        genesis.load_rom(&create_ram_write_rom()).unwrap();

        // Execute breakpoint at the reset PC, gated on a condition that is
        // true at the fetch (pc == 0x400), proves the expression context
        // sees live registers through the real step path.
        let id = genesis.debugger().add_breakpoint(Breakpoint {
            address: 0x00000400,
            bp_type: BreakpointType::execute(),
            cpu_id: Some(0),
            condition: None,
        });
        genesis.debugger().set_breakpoint_match_spec(
            id,
            MatchSpec {
                expr: Some("pc == 0x400".to_string()),
                ..Default::default()
            },
        );

        let result = genesis.step();
        assert!(
            result.breakpoint_hit,
            "conditional execute breakpoint should fire when its expression holds"
        );
        assert_eq!(watch_hits(&genesis, id), 1);
    }

    #[test]
    fn test_conditional_execute_breakpoint_silent_when_expression_false() {
        use retro_core::types::MatchSpec;
        use retro_core::DebuggerCore;

        let mut genesis = GenesisEmulator::new();
        genesis.load_rom(&create_ram_write_rom()).unwrap();

        // Same breakpoint, but the condition can never hold at this pc.
        let id = genesis.debugger().add_breakpoint(Breakpoint {
            address: 0x00000400,
            bp_type: BreakpointType::execute(),
            cpu_id: Some(0),
            condition: None,
        });
        genesis.debugger().set_breakpoint_match_spec(
            id,
            MatchSpec {
                expr: Some("pc == 0x500".to_string()),
                ..Default::default()
            },
        );

        let result = genesis.step();
        assert!(
            !result.breakpoint_hit,
            "conditional execute breakpoint must stay silent when its expression is false"
        );
        assert_eq!(watch_hits(&genesis, id), 0);
    }

    #[test]
    fn test_conditional_watchpoint_reads_memory_in_expression() {
        use retro_core::types::MatchSpec;
        use retro_core::DebuggerCore;

        let mut genesis = GenesisEmulator::new();
        // MOVE.W #$1234 -> ($00FF0000): byte 0x12 lands at 0xFF0000.
        genesis.load_rom(&create_ram_write_rom()).unwrap();

        // Write watch whose expression reads the just-written byte back from
        // memory. The peek happens after the write, so [0xFF0000].b == 0x12.
        let id = genesis.debugger().add_breakpoint(Breakpoint {
            address: 0x00FF0000,
            bp_type: BreakpointType::write(),
            cpu_id: Some(0),
            condition: None,
        });
        genesis.debugger().set_breakpoint_match_spec(
            id,
            MatchSpec {
                expr: Some("[0xFF0000].b == 0x12".to_string()),
                ..Default::default()
            },
        );

        let result = genesis.step();
        assert!(
            result.breakpoint_hit,
            "conditional watchpoint should fire when its memory expression holds"
        );
        assert_eq!(watch_hits(&genesis, id), 1);
    }

    #[test]
    fn test_conditional_watchpoint_silent_when_expression_false() {
        use retro_core::types::MatchSpec;
        use retro_core::DebuggerCore;

        let mut genesis = GenesisEmulator::new();
        genesis.load_rom(&create_ram_write_rom()).unwrap();

        let id = genesis.debugger().add_breakpoint(Breakpoint {
            address: 0x00FF0000,
            bp_type: BreakpointType::write(),
            cpu_id: Some(0),
            condition: None,
        });
        genesis.debugger().set_breakpoint_match_spec(
            id,
            MatchSpec {
                // The byte at 0xFF0000 is 0x12, never 0x99.
                expr: Some("[0xFF0000].b == 0x99".to_string()),
                ..Default::default()
            },
        );

        let result = genesis.step();
        assert!(
            !result.breakpoint_hit,
            "conditional watchpoint must stay silent when its memory expression is false"
        );
        assert_eq!(watch_hits(&genesis, id), 0);
    }

    #[test]
    fn test_run_frame() {
        use retro_core::DebuggerCore;

        let mut genesis = GenesisEmulator::new();
        let rom = create_test_rom();
        genesis.load_rom(&rom).unwrap();

        // Set to running mode
        genesis
            .debugger()
            .set_execution_mode(ExecutionMode::Running);

        assert_eq!(genesis.frame_count(), 0);

        genesis.run_frame();

        assert_eq!(genesis.frame_count(), 1);
        // CPU should have executed many cycles
        assert!(genesis.cpu().total_cycles() > 0);
    }

    #[test]
    fn test_multiple_frames() {
        use retro_core::DebuggerCore;

        let mut genesis = GenesisEmulator::new();
        let rom = create_test_rom();
        genesis.load_rom(&rom).unwrap();

        // Set to running mode
        genesis
            .debugger()
            .set_execution_mode(ExecutionMode::Running);

        for i in 1..=10 {
            genesis.run_frame();
            assert_eq!(genesis.frame_count(), i);
        }
    }

    #[test]
    fn test_region_change() {
        let mut genesis = GenesisEmulator::new();
        assert_eq!(genesis.region(), VideoRegion::Ntsc);

        genesis.set_region(VideoRegion::Pal);
        assert_eq!(genesis.region(), VideoRegion::Pal);
    }

    #[test]
    fn display_info_refresh_rate_tracks_set_region() {
        let mut genesis = GenesisEmulator::new();
        assert!((genesis.get_display_info().refresh_rate - 59.9227).abs() < 1e-4);
        genesis.set_region(VideoRegion::Pal);
        assert!((genesis.get_display_info().refresh_rate - 49.7015).abs() < 1e-4);
        genesis.set_region(VideoRegion::Ntsc);
        assert!((genesis.get_display_info().refresh_rate - 59.9227).abs() < 1e-4);
    }

    fn test_rom_with_regions(regions: &[u8]) -> Vec<u8> {
        let mut rom = create_test_rom();
        // Header region field: three ASCII slots at $1F0..$1F3, space-padded.
        rom[0x1F0..0x1F3].copy_from_slice(b"   ");
        rom[0x1F0..0x1F0 + regions.len()].copy_from_slice(regions);
        rom
    }

    #[test]
    fn load_rom_europe_only_header_selects_pal() {
        for header in [&b"E"[..], &b"8"[..]] {
            let mut genesis = GenesisEmulator::new();
            genesis.load_rom(&test_rom_with_regions(header)).unwrap();
            assert_eq!(genesis.region(), VideoRegion::Pal, "header {header:?}");
            assert!((genesis.get_display_info().refresh_rate - 49.7015).abs() < 1e-4);
        }
    }

    #[test]
    fn load_rom_multi_region_header_stays_ntsc() {
        for header in [&b"JUE"[..], &b"UE"[..], &b"U"[..], &b"   "[..]] {
            let mut genesis = GenesisEmulator::new();
            genesis.load_rom(&test_rom_with_regions(header)).unwrap();
            assert_eq!(genesis.region(), VideoRegion::Ntsc, "header {header:?}");
            assert!((genesis.get_display_info().refresh_rate - 59.9227).abs() < 1e-4);
        }
    }

    #[test]
    fn load_rom_header_region_replaces_a_stale_explicit_region() {
        // A PAL choice left over from a previous session must not leak
        // onto the next cartridge: the header decides on every load.
        let mut genesis = GenesisEmulator::new();
        genesis.set_region(VideoRegion::Pal);
        genesis.load_rom(&test_rom_with_regions(b"JUE")).unwrap();
        assert_eq!(genesis.region(), VideoRegion::Ntsc);
    }

    #[test]
    fn power_on_reset_keeps_an_explicit_region_over_the_header() {
        // The netplay handshake sets the region after load; a cold boot
        // re-applies that override on top of the header's choice.
        let mut genesis = GenesisEmulator::new();
        genesis.load_rom(&test_rom_with_regions(b"E")).unwrap();
        assert_eq!(genesis.region(), VideoRegion::Pal);
        genesis.set_region(VideoRegion::Ntsc);
        genesis.power_on_reset();
        assert_eq!(genesis.region(), VideoRegion::Ntsc);
        assert!((genesis.get_display_info().refresh_rate - 59.9227).abs() < 1e-4);
    }

    #[test]
    fn test_cartridge_audio_mixing() {
        // No Genesis cartridge provides audio (`audio()` is `None`), so this
        // checks the console mix alone: non-empty and in range.
        use retro_core::DebuggerCore;

        let mut genesis = GenesisEmulator::new();
        let rom = create_test_rom();
        genesis.load_rom(&rom).unwrap();

        genesis
            .debugger()
            .set_execution_mode(ExecutionMode::Running);

        // Run a frame to generate audio
        genesis.run_frame();

        // Audio buffer should have samples (PSG + YM2612)
        let audio = genesis.get_audio_buffer();
        assert!(
            !audio.is_empty(),
            "Audio buffer should not be empty after a frame"
        );

        // All samples should be in valid range (-1.0 to 1.0)
        for sample in &audio {
            assert!(
                (-1.0..=1.0).contains(sample),
                "Audio sample out of range: {}",
                sample
            );
        }

        // TODO: check the mix with a cartridge that has audio of its own.
    }

    #[test]
    fn test_z80_integration() {
        let mut genesis = GenesisEmulator::new();

        // Z80 should start in reset
        assert!(genesis.z80_is_reset());

        // Z80 PC should be 0x0000
        assert_eq!(genesis.z80().pc, 0x0000);

        // Take Z80 out of reset
        genesis.set_z80_reset(false);
        assert!(!genesis.z80_is_reset());

        // Z80 should have default state
        assert_eq!(genesis.z80().sp, 0xFFFF);
        assert_eq!(genesis.z80().a, 0);
    }

    #[test]
    fn test_z80_bus_request() {
        let mut genesis = GenesisEmulator::new();

        // Initially no bus request
        assert!(!genesis.z80_bus_requested());

        // Request Z80 bus
        genesis.set_z80_bus_request(true);
        assert!(genesis.z80_bus_requested());

        // Release bus
        genesis.set_z80_bus_request(false);
        assert!(!genesis.z80_bus_requested());
    }

    #[test]
    fn test_z80_reset() {
        let mut genesis = GenesisEmulator::new();

        // Take Z80 out of reset and modify state
        genesis.set_z80_reset(false);
        genesis.z80_mut().a = 0x42;
        genesis.z80_mut().pc = 0x1234;

        // Reset should clear state
        genesis.reset();
        assert!(genesis.z80_is_reset());
        assert_eq!(genesis.z80().pc, 0x0000);
        assert_eq!(genesis.z80().a, 0);
    }

    #[test]
    fn load_rom_leaves_the_reset_state_in_the_debugger() {
        use retro_core::DebuggerCore;

        // No step and no frame: the platform's debugger reads the state the
        // moment a project's Build & Run has loaded the ROM.
        let mut genesis = GenesisEmulator::new();
        genesis.load_rom(&create_test_rom()).unwrap();

        let cpu_states = genesis.debugger().get_cpu_states();
        assert_eq!(cpu_states.len(), 2);
        assert_eq!(cpu_states[0].cpu_id, 0);
        assert_eq!(cpu_states[0].pc, 0x0000_0400);
        assert_eq!(cpu_states[1].cpu_id, 1);
    }

    #[test]
    fn test_debugger_shows_both_cpus() {
        use retro_core::DebuggerCore;

        let mut genesis = GenesisEmulator::new();
        let rom = create_test_rom();
        genesis.load_rom(&rom).unwrap();

        // Update debugger state to populate CPU states
        genesis.update_debugger_state();

        // Get CPU states from debugger
        let cpu_states = genesis.debugger().get_cpu_states();

        // Should have 2 CPUs
        assert_eq!(cpu_states.len(), 2);

        // First CPU should be 68K (ID 0)
        assert_eq!(cpu_states[0].cpu_id, 0);
        assert!(!cpu_states[0].registers.is_empty());

        // Second CPU should be Z80 (ID 1)
        assert_eq!(cpu_states[1].cpu_id, 1);
        assert!(!cpu_states[1].registers.is_empty());

        // Check Z80 has expected registers
        let z80_regs: Vec<String> = cpu_states[1]
            .registers
            .iter()
            .map(|r| r.name.clone())
            .collect();

        assert!(z80_regs.contains(&"A".to_string()));
        assert!(z80_regs.contains(&"PC".to_string()));
        assert!(z80_regs.contains(&"SP".to_string()));
    }

    // --------------------------------------------------------------------
    // EmulatorCore::set_input: touch overlay / virtual-input bridge
    //
    // The JS-side touch overlay calls `setInput(player, mask)`. `set_input`
    // routes `InputState.buttons` through `InputManager` as a direct
    // override that OR-merges with the assigned provider's state, so touch +
    // keyboard + gamepad all stack onto the same port and a press reaches
    // the I/O ports.
    // --------------------------------------------------------------------

    #[test]
    fn test_set_input_drives_port_state() {
        use retro_core::{EmulatorCore, InputState};

        let mut genesis = GenesisEmulator::new();
        // Genesis A is bit 6 (the layout `set_port_button_state` documents).
        genesis.set_input(0, InputState { buttons: 1 << 6 });

        // The port state reports A as pressed.
        assert_eq!(
            genesis.input_manager().get_port_state(0),
            1 << 6,
            "set_input must drive get_port_state: the touch overlay relies on this path",
        );
    }

    #[test]
    fn test_set_input_combines_with_keyboard_provider() {
        use retro_core::{EmulatorCore, InputState};

        let mut genesis = GenesisEmulator::new();

        // Keyboard arrow-up presses Genesis UP (bit 0) via the default
        // keyboard mapping in `InputManager::new()`.
        genesis
            .input_manager_mut()
            .update_keyboard("ArrowUp".to_string(), true);
        let keyboard_only = genesis.input_manager().get_port_state(0);
        assert_ne!(keyboard_only, 0, "keyboard alone must produce input");

        // Touch overlay simulates A (bit 6). Both must be visible to the
        // I/O ports: there's no reason a player on mobile shouldn't be
        // allowed to use both at once if they happen to plug in a keyboard.
        genesis.set_input(0, InputState { buttons: 1 << 6 });
        assert_eq!(
            genesis.input_manager().get_port_state(0),
            keyboard_only | (1 << 6),
            "set_input must OR with the keyboard provider, not replace it",
        );

        // Releasing the touch (mask = 0) leaves the keyboard alone.
        genesis.set_input(0, InputState { buttons: 0 });
        assert_eq!(genesis.input_manager().get_port_state(0), keyboard_only);
    }

    #[test]
    fn test_set_input_player_2_targets_port_1() {
        use retro_core::{EmulatorCore, InputState};

        let mut genesis = GenesisEmulator::new();
        genesis.set_input(1, InputState { buttons: 1 << 7 }); // START

        assert_eq!(genesis.input_manager().get_port_state(0), 0);
        assert_eq!(genesis.input_manager().get_port_state(1), 1 << 7);
    }

    #[test]
    fn read_memory_debug_leaves_the_vdp_ports_alone() {
        let mut genesis = GenesisEmulator::new();
        let bus = genesis.bus_mut();
        bus.write_word(0xC00004, 0x8F02); // auto-increment 2
                                          // The first half of a VRAM write command at address 0: the VDP now
                                          // waits for the second word.
        bus.write_word(0xC00004, 0x4000);

        // A memory view over the ports must not cancel that wait, as a CPU
        // read of the control port would.
        let view = genesis.read_memory_debug(0xC00000, 0x20);
        assert!(view.iter().all(|&b| b == 0));

        let bus = genesis.bus_mut();
        bus.write_word(0xC00004, 0x0000);
        bus.write_word(0xC00000, 0x1234);
        assert_eq!(&genesis.vdp().get_vram()[0..2], &[0x12, 0x34]);
    }

    #[test]
    fn disassembly_views_leave_the_vdp_ports_alone() {
        let mut genesis = GenesisEmulator::new();
        let bus = genesis.bus_mut();
        bus.write_word(0xC00004, 0x8F02); // auto-increment 2
        bus.write_word(0xC00004, 0x4000); // first half of a VRAM write at 0

        // The debugger's disassembly, pointed at the ports, must not read
        // them as the 68000 would.
        let rows = genesis.disassemble_at(0xC00000, 8);
        assert_eq!(rows.len(), 8);
        let rows = genesis.disassemble_around_pc(0xC00004, 0, 8);
        assert_eq!(rows.len(), 8);

        let bus = genesis.bus_mut();
        bus.write_word(0xC00004, 0x0000);
        bus.write_word(0xC00000, 0x1234);
        assert_eq!(&genesis.vdp().get_vram()[0..2], &[0x12, 0x34]);
    }
}
