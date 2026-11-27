use retro_core::audio::RateConverterState;
use retro_core::savestate::{
    part_hash, LoadStateError, RomCheck, RomIdentity, SavestateFormat, SavestatePayload,
};
use serde::{Deserialize, Serialize};

/// Version 3 carries the cartridge's state as the typed `cart_sram` +
/// `cart_coprocessors` pair; the opaque `cartridge` blob before them is
/// always written empty.
///
/// Every field of this layout is read from the stream (bincode), so no
/// `#[serde(default)]` here fills one in: a savestate written before a field
/// was added is misread from that field on.
pub const SAVE_STATE_VERSION: u32 = 3;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SystemSaveState {
    pub version: u32,
    pub metadata: SaveStateMetadata,
    pub cpu68k: Cpu68kState,
    pub z80: Z80State,
    pub vdp: VdpState,
    pub psg: PsgState,
    pub ym2612: Ym2612State,
    pub io: IoState,
    pub bus: BusState,
    pub timing: SystemTimingState,
    pub ram: Vec<u8>,
    pub z80_ram: Vec<u8>,
    /// Opaque cartridge blob of the savestates before version 3. Always
    /// written empty; it stays in the layout so the savestates written with
    /// it keep deserializing.
    #[serde(default)]
    pub cartridge: Vec<u8>,
    /// Battery-SRAM image (composable `GenesisCartridge::sram`).
    #[serde(default)]
    pub cart_sram: Vec<u8>,
    /// Typed per-coprocessor state, keyed by `chip_id`.
    #[serde(default)]
    pub cart_coprocessors: Vec<(u8, GenesisChipState)>,
    pub frame_count: u64,
    pub screenshot: Option<Vec<u8>>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SaveStateMetadata {
    pub game_title: String,
    pub rom_hash: String,
    pub timestamp: String,
    pub slot: u8,
    pub emulator_version: String,
}

// 68K CPU
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Cpu68kState {
    pub d: [u32; 8],
    pub a: [u32; 8],
    pub pc: u32,
    pub sr: u16,
    pub ssp: u32,
    pub usp: u32,
    pub cycles: u64,
    pub stopped: bool,
    pub pending_irq_level: u8,
}

// Z80 CPU
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Z80State {
    pub a: u8,
    pub f: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub e: u8,
    pub h: u8,
    pub l: u8,
    pub a_prime: u8,
    pub f_prime: u8,
    pub b_prime: u8,
    pub c_prime: u8,
    pub d_prime: u8,
    pub e_prime: u8,
    pub h_prime: u8,
    pub l_prime: u8,
    pub ix: u16,
    pub iy: u16,
    pub sp: u16,
    pub pc: u16,
    pub i: u8,
    pub r: u8,
    pub wz: u16,
    pub iff1: bool,
    pub iff2: bool,
    pub im: u8,
    pub ei: u8,
    pub p: u8,
    pub q: u8,
    pub prev_q: u8,
    pub halted: bool,
    pub cycles: u64,
}

// VDP
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct VdpState {
    pub vram: Vec<u8>,
    pub cram: Vec<u16>,
    pub vsram: Vec<u16>,
    pub registers: [u8; 24],
    pub control_word: u32,
    pub pending_write: bool,
    pub address: u16,
    pub code: u8,
    pub timing: VdpTimingState,
    pub dma_fill_value: u16,
    pub dma_pending: bool,
    pub dma_busy: bool,
    pub dma_remaining_clocks: u64,
    /// Reserved, always written `false`: the metered Fill/Copy busy model needs
    /// no "DMA spanned VBLANK" override, but the field stays in the bincode
    /// layout so existing savestates keep deserializing.
    pub dma_crosses_vblank: bool,
    pub sprite_overflow: bool,
    pub sprite_collision: bool,
    pub sprite_mask_overflow: bool,
    pub odd_frame: bool,
    pub fifo_entries: u8,
    pub fifo_mclk_counter: u64,
    /// Master clocks the 68000 still owes for an armed 68k-bus DMA whose
    /// stall spilled past the frame boundary (the frame loop meters it into
    /// the per-line CPU budget, so a DMA armed late in a frame carries a
    /// non-zero remainder into the next). Cross-frame CPU-timing state:
    /// without it a restored instance skips the stall and runs the CPU ahead
    /// of the live one, a rollback desync (Gunstar Heroes reproduced it).
    #[serde(default)]
    pub pending_cpu_dma_mclks: u64,
    /// VDP data-port read prefetch latch. A control-port read setup loads it;
    /// each data-port read returns it and prefetches the next word. A game can
    /// arm the read on one frame and consume it on the next, so a restored
    /// instance that defaults it to `0` hands back the wrong word and diverges.
    #[serde(default)]
    pub read_buffer: u16,
    /// Frozen H/V-counter value while reg0 bit1 (HV latch) is set: H/V counter
    /// reads return this instead of the live beam position until the game clears
    /// the bit, so it stays latched across frames. Dropping it makes a restored
    /// instance return the live counter, the desync VDPFIFOTesting's "HV Counter
    /// Latch" subtest and Sunset Riders' raster effect trip on. `None` = unlatched.
    #[serde(default)]
    pub hvc_latch: Option<u16>,
    /// The four-word FIFO data ring plus its next-write index. Partial-width
    /// data-port reads OR the "open bus" bits in from `fifo[fifo_idx]`, and a
    /// DMA fill sources its value from the ring, so the ring is machine state a
    /// game can observe a frame after the writes that filled it. `fifo_idx` is
    /// stored as a `u8` (0..=3) to keep the layout width-stable across targets.
    #[serde(default)]
    pub fifo: [u16; 4],
    #[serde(default)]
    pub fifo_idx: u8,
    /// FIFO service clock and per-slot drain timestamps (absolute VDP master
    /// clocks). The status FIFO-empty (bit 9) / full (bit 8) bits compare
    /// `fifo_clock` against `fifo_slot_mclks`, so a FIFO caught mid-drain at a
    /// frame boundary reports the wrong status on a restored instance unless
    /// both are carried. Saved and restored together, so only their relative
    /// ordering (all the status bits depend on) has to hold, and it does.
    #[serde(default)]
    pub fifo_clock: u64,
    #[serde(default)]
    pub fifo_slot_mclks: [u64; 4],
    /// One-shot "DMA fill armed" flag: the control-port write that selects fill
    /// mode sets it and the following data-port write consumes it to start the
    /// fill. The two writes can straddle a savestate, so a restored instance
    /// that defaults it to `false` silently never starts the fill.
    #[serde(default)]
    pub dma_fill_pending: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct VdpTimingState {
    pub region: u8,
    pub scanline: u16,
    pub h_counter: u16,
    pub frame_cycles: u32,
    pub frame_count: u64,
    pub in_vblank: bool,
    pub in_hblank: bool,
    pub vblank_pending: bool,
    pub vint_irq_pending: bool,
    pub hblank_pending: bool,
    pub h_counter_reload: u8,
    pub h_counter_current: u8,
}

// PSG
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PsgState {
    pub tone_channels: [ToneChannelState; 3],
    pub noise_channel: NoiseChannelState,
    pub latched_channel: u8,
    pub latched_type: u8,
    pub clock_accumulator: f32,
    pub internal_clock_accumulator: f32,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ToneChannelState {
    pub frequency: u16,
    pub attenuation: u8,
    pub counter: u16,
    pub polarity: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct NoiseChannelState {
    pub shift_rate: u8,
    pub mode: u8,
    pub lfsr: u16,
    pub attenuation: u8,
    pub counter: u16,
    pub polarity: bool,
    pub tone3_frequency: u16,
}

// YM2612
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Ym2612State {
    pub channels: [Ym2612ChannelState; 6],
    pub lfo_enabled: bool,
    pub lfo_freq: u8,
    pub lfo_phase: u8,
    pub lfo_counter: u32,
    pub dac_enabled: bool,
    pub dac_value: i16,
    pub dac_start_value: i16,
    pub clock_accumulator: f32,
    pub timer_accumulator: f32,
    pub ch3_mode: u8,
    pub ch3_fnum: [u16; 4],
    pub ch3_block: [u8; 4],
    pub csm_key_state: u8,
    pub address_latch_part0: u8,
    pub address_latch_part1: u8,
    pub eg_accumulator: f32,
    pub eg_counter: u32,
    pub busy_end_cycle: u64,
    pub current_cpu_cycle: u64,
    pub timer: Ym2612TimerState,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Ym2612ChannelState {
    pub operators: [Ym2612OperatorState; 4],
    pub algorithm: u8,
    pub feedback: u8,
    pub op1_prev: [i16; 2],
    pub stereo: u8,
    pub pms: u8,
    pub fnum: u16,
    pub block: u8,
    pub ch3_special: bool,
    pub op_fnum: [u16; 4],
    pub op_block: [u8; 4],
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Ym2612OperatorState {
    pub freq_number: u16,
    pub block: u8,
    pub detune: u8,
    pub multiple: u8,
    pub phase: u32,
    pub phase_increment: u32,
    pub total_level: u8,
    pub am_enable: bool,
    pub ams: u8,
    pub rate_scaling: u8,
    pub key_code: u8,
    pub envelope: EnvelopeState,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EnvelopeState {
    pub attack_rate: u8,
    pub decay_rate: u8,
    pub sustain_level: u8,
    pub sustain_rate: u8,
    pub release_rate: u8,
    pub state: u8,
    pub level: u16,
    pub counter: u32,
    pub key_on: bool,
    pub key_code: u8,
    pub rate_scaling: u8,
    pub ssg_eg_enable: bool,
    pub ssg_eg_mode: u8,
    pub ssg_eg_inverted: bool,
    pub ssg_phase_reset: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Ym2612TimerState {
    pub ta_period: u16,
    pub ta_load: i32,
    pub ta_counter: i32,
    pub tb_period: u8,
    pub tb_load: i32,
    pub tb_counter: i32,
    pub mode: u8,
    pub status: u8,
    pub timer_a_overflowed_since_check: bool,
}

// I/O
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct IoState {
    pub data_registers: [u8; 3],
    pub control_registers: [u8; 3],
    pub port_cycles: [u32; 3],
    pub th_toggle_counts: [u32; 3],
}

// Bus state
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BusState {
    pub z80_bus_req: bool,
    pub z80_reset: bool,
    pub z80_reset_release_pending: bool,
    pub z80_bank_register: u32,
    pub z80_bank_bit_count: u8,
    pub cycles_executed: u64,
}

// System timing
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SystemTimingState {
    pub master_cycles: u64,
    /// Deprecated `f64` audio-sample accumulator. Kept in the struct so
    /// the layout, and the savestates that carry the field, stay readable;
    /// the loader ignores the value (the converter comes from
    /// [`audio_rate_converter`]). New savestates always write `0.0`.
    #[serde(default)]
    pub audio_sample_accumulator: f64,
    pub audio_enabled: bool,
    pub last_step_cycles: u32,
    pub z80_irq_pending: bool,
    pub step_z80_target: u32,
    pub step_z80_executed: u32,
    pub step_z80_credit_numer: u32,
    pub step_ym_cycles_clocked: u32,
    pub step_master_clocks_accumulated: u64,
    /// Snapshot of the active [`retro_core::IntegerRateConverter`].
    /// The loader treats the all-zero value (a fresh emulator's, before
    /// anything anchored it) as no captured state and anchors the
    /// converter at `master_cycles` instead (equivalent to `reset()`
    /// aligned to the restored emulation time, which is correct for
    /// continuing playback after a state restore).
    #[serde(default)]
    pub audio_rate_converter: RateConverterState,
    /// Fractional-cycle carry for the per-scanline 68K budget (numerator over
    /// the CPU divider).
    #[serde(default)]
    pub scanline_cycle_carry: u32,
    /// 68K instruction overshoot (68K cycles) carried across the frame
    /// boundary.
    #[serde(default)]
    pub frame_cycle_overshoot: u32,
    /// Absolute 68K-cycle target for the next DRAM-refresh bus-steal. A `0`
    /// (no running emulator writes one) is re-anchored by `load_state` to the
    /// next period boundary at once.
    #[serde(default)]
    pub bus_refresh_target: u64,
}

// The cartridge's chips

/// Which dynamic banking scheme a bank-mapper chip implements. The chip
/// owns the cartridge ROM and translates 68000 ROM-window reads through a
/// per-slot bank table; this tag records which register protocol drives
/// that table so a restore rebinds the right one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BankMapperKind {
    /// SEGA mapper (8 x 512 KiB slots; the bank registers are the odd
    /// addresses `0xA130F3..0xA130FF`, for slots 1-7; slot 0 holds bank 0).
    Sega,
    /// Super Street Fighter II (`0xA130F1` toggles bank 7/8 into slot 7).
    Ssf2,
    /// Codemasters (bank selects written into ROM-window addresses).
    Codemasters,
}

/// Volatile state of a bank-switching mapper chip: the per-slot bank
/// selection. SEGA carries 8 entries (one per 512 KiB slot), Codemasters
/// 4, SSF2 a single entry (the high bank latched into slot 7). The chip
/// re-derives `bank_count` from the ROM it is handed on restore, so only
/// the live selection is persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BankMapperState {
    pub kind: BankMapperKind,
    pub banks: Vec<u8>,
}

/// Serial-EEPROM protocol phase (I2C-like state machine). Mirror of the
/// chip's internal enum so the exact handshake position round-trips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EepromPhase {
    Idle,
    DeviceAddress,
    DeviceAck,
    MemoryAddress,
    MemoryAck,
    WriteData,
    WriteAck,
    ReadData,
    ReadAck,
    Stop,
}

/// Volatile state of a serial EEPROM chip (X24C01/X24C02). `data` is the
/// battery-backed contents (also surfaced as the cartridge's save RAM);
/// the remaining fields are the mid-transfer protocol state so a savestate
/// taken during a read/write handshake resumes cleanly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EepromChipState {
    pub data: Vec<u8>,
    pub phase: EepromPhase,
    pub sda: bool,
    pub scl: bool,
    pub sda_prev: bool,
    pub scl_prev: bool,
    pub bit_count: u8,
    pub current_byte: u8,
    pub device_addr: u8,
    pub rw_bit: bool,
    pub memory_addr: u8,
}

/// Volatile state of a J-Cart controller-port expander. Only the latched
/// port/TH registers persist; the connected controllers' button state is
/// driven live by the input layer each frame, not saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct JCartState {
    pub port3_data: u8,
    pub port4_data: u8,
    pub port3_th: bool,
    pub port4_th: bool,
}

/// Volatile state of the SVP coprocessor.
///
/// The SVP shares a block of DRAM with the 68000 and runs an SSP1601 DSP out
/// of internal RAM. This snapshot persists that shared DRAM plus the entire
/// SSP1601 register/RAM/stack/PMAC file, including the mailbox registers
/// (`SSP_XST`/`SSP_PM0` live in `gr`) and the wait-loop flags (`emu_status`),
/// so a savestate taken mid-handshake or mid-render resumes cleanly. The
/// program ROM half of the SSP's instruction memory is *not* saved: it is
/// re-copied from the cartridge on restore, so only the writable `iram`
/// (words `0x000..0x400`) is persisted. `last_master_cycle`/`ssp_accum` are
/// the coprocessor's catch-up anchor (in 68K cycles) and its carry. Kept
/// `Default` so the chip round-trips an empty snapshot with no special-casing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SvpState {
    /// Shared 68000/SSP DRAM (`0x300000-0x31FFFF`), big-endian bytes.
    pub dram: Vec<u8>,
    /// SSP1601 general register file (`gr[16]`; 16-bit regs in the high half,
    /// `A`/`P`/`PMC` full 32-bit). Carries `SSP_XST`/`SSP_PM0`/`SSP_PC`.
    pub gr: [u32; 16],
    /// Two internal RAM banks (512 words).
    pub ram: Vec<u16>,
    /// Eight 8-bit pointer registers.
    pub ptr: [u8; 8],
    /// Six-level hardware stack.
    pub stack: [u16; 6],
    /// PMAC read/write address+mode file for PM0-PM5.
    pub pmac: [[u32; 6]; 2],
    /// PMC programming state + wait-loop flags (`SSP_WAIT_*`).
    pub emu_status: u32,
    /// Writable IRAM (instruction-memory words `0x000..0x400`); the program
    /// ROM half is reconstructed from the cartridge on restore.
    pub iram: Vec<u16>,
    /// SSP1601 cycle counter (diagnostic).
    pub cycles: u64,
    /// 68K-cycle anchor of the last catch-up (`None` before the first).
    pub last_master_cycle: Option<u64>,
    /// Fractional master-cycle->SSP-cycle carry across catch-ups.
    pub ssp_accum: u64,
}

/// A cartridge chip's state in a savestate. The generic
/// [`Coprocessor`](retro_core::cartridge::Coprocessor) trait saves a chip
/// as opaque bytes; each chip writes one variant of this enum in them, and
/// the cartridge (its board names this enum as its chip state) decodes it
/// back, keyed by `chip_id` in the payload. The variant tags the chip, so a
/// blob one chip saved never restores into another; variants are only
/// appended, so the indices stay and older savestates keep decoding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GenesisChipState {
    BankMapper(BankMapperState),
    Eeprom(EepromChipState),
    JCart(JCartState),
    Svp(SvpState),
}

// Validation & helpers
impl SystemSaveState {
    pub fn size(&self) -> usize {
        bincode::serialized_size(self).unwrap_or(0) as usize
    }
}

impl SavestatePayload for SystemSaveState {
    retro_core::savestate_codec!();

    const FORMAT: SavestateFormat = SavestateFormat {
        core_id: "sg1",
        version: SAVE_STATE_VERSION,
    };
    type Extension = GenesisExtension;

    /// A newer layout and memories of the wrong size are refused.
    fn validate(&self) -> Result<(), String> {
        if self.version > SAVE_STATE_VERSION {
            return Err(format!(
                "Save state version {} is newer than supported version {}",
                self.version, SAVE_STATE_VERSION
            ));
        }
        if self.ram.len() != 65536 {
            return Err(format!(
                "Invalid RAM size: {} (expected 65536)",
                self.ram.len()
            ));
        }
        if self.z80_ram.len() != 8192 {
            return Err(format!(
                "Invalid Z80 RAM size: {} (expected 8192)",
                self.z80_ram.len()
            ));
        }
        if self.vdp.vram.len() != 65536 {
            return Err(format!(
                "Invalid VRAM size: {} (expected 65536)",
                self.vdp.vram.len()
            ));
        }
        Ok(())
    }

    /// The payload's parts by chip and memory; `frame_count` as itself.
    fn hash_parts(&self) -> Vec<(&'static str, u64)> {
        vec![
            ("cpu68k", part_hash(&self.cpu68k)),
            ("z80", part_hash(&self.z80)),
            ("vdp", part_hash(&self.vdp)),
            ("psg", part_hash(&self.psg)),
            ("ym2612", part_hash(&self.ym2612)),
            ("io", part_hash(&self.io)),
            ("bus", part_hash(&self.bus)),
            ("timing", part_hash(&self.timing)),
            ("ram", part_hash(&self.ram)),
            ("z80_ram", part_hash(&self.z80_ram)),
            ("cartridge", part_hash(&self.cartridge)),
            ("frame_count", self.frame_count),
        ]
    }

    /// The form before the envelope: bincode and zstd of the payload alone.
    /// It names its ROM by hash and title, and the refusal names the titles,
    /// the only names it ever showed.
    fn open_without_envelope(
        bytes: &[u8],
        rom: Option<RomIdentity<'_>>,
        check: RomCheck,
    ) -> Result<Self, LoadStateError> {
        let state = decompress_and_deserialize(bytes)?;
        if let (RomCheck::SameRom, Some(rom)) = (check, rom) {
            if !state.metadata.rom_hash.is_empty() && rom.sha256 != state.metadata.rom_hash {
                return Err(LoadStateError::OtherRom {
                    state_rom: state.metadata.game_title.to_string(),
                    loaded_rom: rom.title.to_string(),
                });
            }
        }
        Ok(state)
    }
}

// After the payload

/// What a savestate holds after the payload: machine state the layout has
/// no field for, each part at its none when the machine holds none of it,
/// so a state taken where a frame ends with nothing of the kind keeps the
/// layout's bytes. Fields are only ever appended
/// (`SavestateExtension::VERSION` counts the sets).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct GenesisExtension {
    pub vdp: VdpExtension,
    pub ym2612: Ym2612Extension,
    pub psg: PsgExtension,
    /// The 68000 cycle the Z80's bus is granted at, while a request waits
    /// for it; 0 when none waits.
    pub z80_bus_grant: u64,
    /// A frame stopped part of the way through: where it stands.
    pub frame: Option<GenesisFrameState>,
}

impl retro_core::savestate::SavestateExtension for GenesisExtension {
    const VERSION: u32 = 1;
}

/// The VDP's part of [`GenesisExtension`].
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct VdpExtension {
    /// A VRAM, CRAM or VSRAM fill still writing its cells.
    pub fill: Option<VdpFillState>,
    /// The master clocks left before the vertical interrupt is requested of
    /// the 68000, while the request is held off into the line vertical
    /// blanking starts on.
    pub vint_delay: Option<u32>,
    /// The first frame after a reset, whose vertical blank raises no
    /// interrupt, not yet past it.
    pub first_frame_after_reset: bool,
    /// Where the beam stands when that is not a line's end; a load puts the
    /// beam of an ended line back from the payload.
    pub beam: Option<VdpBeamState>,
    /// The sprite list read from the sprite table for the frame being drawn,
    /// once its first line with sprites has read it.
    pub sprites: Option<Vec<VdpSpriteState>>,
}

/// A fill in flight: the cells it has still to write, and the carry of
/// the master clocks it has run, in cells times clocks.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct VdpFillState {
    pub remaining: u32,
    pub carry: u64,
}

/// The beam in its line, in 68000 cycles: how far in, and the line's
/// length.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct VdpBeamState {
    pub into_line: u32,
    pub line_length: u32,
}

impl VdpBeamState {
    /// Whether the beam stands at the end of its line. Every place the
    /// beam is ended there reads it the same, whatever the two values.
    pub fn at_line_end(&self) -> bool {
        self.line_length > 0 && self.into_line >= self.line_length
    }
}

/// One sprite of the sprite list, as the sprite table gave it.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct VdpSpriteState {
    pub x: i16,
    pub y: i16,
    pub width_tiles: u8,
    pub height_tiles: u8,
    pub pattern_base: u16,
    pub palette: u8,
    pub priority: bool,
    pub flip_h: bool,
    pub flip_v: bool,
    pub link: u8,
}

/// The YM2612's part of [`GenesisExtension`].
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Ym2612Extension {
    /// The busy time left after a write, in the chip's input clocks.
    pub busy: u32,
    /// The samples made so far in the frame, which its end takes.
    pub samples: Vec<f32>,
}

/// The PSG's part of [`GenesisExtension`].
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct PsgExtension {
    /// The samples made so far in the frame, which its end takes.
    pub samples: Vec<f32>,
}

/// A frame stopped part of the way through.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct GenesisFrameState {
    /// The line the frame stands on, and whether that line's start ran.
    pub line: u32,
    pub line_started: bool,
    /// The line's counters once its start ran (all 0 before): the clock
    /// less the 68000 cycles the line has spent, the line's 68000 budget,
    /// the PSG's credit and its clocks given in the line, the Z80's target,
    /// credit and cycles run, the YM2612's clocks given. The PSG's credit is
    /// its clock's, which a load derives from the master clock; a build
    /// that counted the credit and the clocks within the line reads them as
    /// its own.
    pub grid: u64,
    pub budget: u32,
    pub psg_credit: u32,
    pub psg_clocked: u32,
    pub z80_target: u32,
    pub z80_credit: u32,
    pub z80_executed: u32,
    pub ym_clocked: u32,
    /// A step stopped before its instruction, the part before it run: the
    /// cycles of the interrupt that part took.
    pub stopped_step: Option<u32>,
    /// The buttons each pad held when the frame started.
    pub input: [u32; 2],
}

impl SaveStateMetadata {
    pub fn now(game_title: String, rom_hash: String, slot: u8) -> Self {
        Self {
            game_title,
            rom_hash,
            timestamp: chrono::Utc::now().to_rfc3339(),
            slot,
            emulator_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

/*
 * Save State Implementation
 *
 * Compression and serialization helpers
 */

/// Compress data using zstd
fn compress_zstd(data: &[u8], level: i32) -> Result<Vec<u8>, String> {
    zstd::encode_all(data, level).map_err(|e| format!("Compression failed: {}", e))
}

/// Decompress data using zstd
fn decompress_zstd(data: &[u8]) -> Result<Vec<u8>, String> {
    zstd::decode_all(data).map_err(|e| format!("Decompression failed: {}", e))
}

/// Serialize and compress a save state
pub fn serialize_and_compress(
    state: &SystemSaveState,
    compression_level: i32,
) -> Result<Vec<u8>, String> {
    // Serialize to bincode
    let serialized =
        bincode::serialize(state).map_err(|e| format!("Serialization failed: {}", e))?;

    // Compress
    compress_zstd(&serialized, compression_level)
}

/// Decompress and deserialize a save state of the form before the envelope.
fn decompress_and_deserialize(data: &[u8]) -> Result<SystemSaveState, String> {
    // Decompress
    let decompressed = decompress_zstd(data)?;

    // Deserialize
    let state: SystemSaveState = bincode::deserialize(&decompressed)
        .map_err(|e| format!("Deserialization failed: {}", e))?;

    // Validate
    SavestatePayload::validate(&state)?;

    // Check version
    if state.version > SAVE_STATE_VERSION {
        return Err(format!(
            "Save state version {} is newer than supported version {}",
            state.version, SAVE_STATE_VERSION
        ));
    }

    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compression_roundtrip() {
        let data = b"Hello, world! This is a test of zstd compression.";
        let compressed = compress_zstd(data, 3).unwrap();
        let decompressed = decompress_zstd(&compressed).unwrap();
        assert_eq!(data.as_slice(), decompressed.as_slice());
    }

    #[test]
    fn test_compression_reduces_size() {
        let data = vec![0u8; 10000]; // Highly compressible
        let compressed = compress_zstd(&data, 3).unwrap();
        assert!(compressed.len() < data.len());
        println!(
            "Original: {} bytes, Compressed: {} bytes",
            data.len(),
            compressed.len()
        );
    }

    #[test]
    fn bank_mapper_state_round_trips_via_bincode() {
        let state = GenesisChipState::BankMapper(BankMapperState {
            kind: BankMapperKind::Sega,
            banks: vec![0, 1, 2, 3, 4, 5, 6, 7],
        });
        let bytes = bincode::serialize(&state).unwrap();
        let back: GenesisChipState = bincode::deserialize(&bytes).unwrap();
        assert_eq!(state, back);
    }

    #[test]
    fn eeprom_state_round_trips_via_bincode() {
        let state = GenesisChipState::Eeprom(EepromChipState {
            data: vec![0xAB; 128],
            phase: EepromPhase::WriteData,
            sda: false,
            scl: true,
            sda_prev: true,
            scl_prev: false,
            bit_count: 3,
            current_byte: 0x5A,
            device_addr: 0x50,
            rw_bit: true,
            memory_addr: 0x10,
        });
        let bytes = bincode::serialize(&state).unwrap();
        assert_eq!(
            bincode::deserialize::<GenesisChipState>(&bytes).unwrap(),
            state
        );
    }

    #[test]
    fn jcart_and_svp_states_round_trip() {
        let jcart = GenesisChipState::JCart(JCartState {
            port3_data: 0x40,
            port4_data: 0x00,
            port3_th: true,
            port4_th: false,
        });
        let svp = GenesisChipState::Svp(SvpState {
            dram: vec![1, 2, 3],
            gr: [0x1234_5678; 16],
            ram: vec![0xAAAA; 512],
            ptr: [1, 2, 3, 4, 5, 6, 7, 0],
            stack: [10, 20, 30, 40, 50, 60],
            pmac: [[1, 2, 3, 4, 5, 6], [7, 8, 9, 10, 11, 12]],
            emu_status: 0x2000,
            iram: vec![0x0F0F; 0x400],
            cycles: 999,
            last_master_cycle: Some(123_456),
            ssp_accum: 42,
        });
        for state in [jcart, svp] {
            let bytes = bincode::serialize(&state).unwrap();
            assert_eq!(
                bincode::deserialize::<GenesisChipState>(&bytes).unwrap(),
                state
            );
        }
    }

    #[test]
    fn variants_are_backend_tagged_distinctly() {
        // A J-Cart blob must not decode as (or equal) an SVP state, the
        // tag keeps chips from cross-loading each other's bytes.
        let jcart = GenesisChipState::JCart(JCartState::default());
        let svp = GenesisChipState::Svp(SvpState::default());
        assert_ne!(jcart, svp);
    }
}
