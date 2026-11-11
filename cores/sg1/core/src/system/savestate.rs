use serde::{Deserialize, Serialize};

use crate::cartridge::composable::GenesisCoprocessorState;

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
    pub cart_coprocessors: Vec<(u8, GenesisCoprocessorState)>,
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
    pub audio_rate_converter: SerializableConverterState,
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

/// Persisted state of the [`retro_core::IntegerRateConverter`] inside
/// `SystemTimingState`. The all-zero `Default` is reserved as a
/// sentinel meaning "no captured state": the loader treats it as
/// "anchor at master_cycles + reset". A genuinely fresh converter
/// after a `reset(master_cycles)` call will have `reference_cycles ==
/// master_cycles` (non-zero on any running emulator), so the sentinel
/// is unambiguous.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct SerializableConverterState {
    pub reference_cycles: u64,
    pub samples_at_reference: u64,
    pub remainder: u64,
}

// Validation & helpers
impl SystemSaveState {
    pub fn size(&self) -> usize {
        bincode::serialized_size(self).unwrap_or(0) as usize
    }

    pub fn validate(&self) -> Result<(), String> {
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
