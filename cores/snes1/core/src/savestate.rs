//! Save-state serialisation.
//!
//! Snapshots all volatile state owned by [`crate::system::SnesEmulator`]
//! (CPU registers, WRAM, ARAM, VRAM/CGRAM/OAM, PPU latches, scheduler
//! cursor, DMA channel state, controller shift registers, battery
//! SRAM) and packs it through the shared
//! [`retro_core::savestate::SaveStateEnvelope`] so the SG1 and SNES1
//! cores share the same compressed on-disk format and forward-compat
//! rules.
//!
//! ## Snapshot DTOs, not live structs
//!
//! Every component is mirrored by a dedicated `*State` data-transfer
//! object here; the live runtime structs never derive `Serialize`
//! themselves. That keeps the wire format decoupled from the runtime
//! layout (caches, host-side buffers, and the external CPU crates'
//! registers all stay out of the format), and lets each component own
//! a `to_savestate()` / `from_savestate()` pair next to its own
//! private fields. Big memories (WRAM/VRAM/OAM/CGRAM/ARAM, the DSP
//! register file) become `Vec` in the DTO; everything bounded stays a
//! fixed array so `bincode` keeps it compact.
//!
//! ## What is *not* captured
//!
//! - The cartridge, its [`crate::cartridge::MapPlan`], the page/speed
//!   tables and the FastROM page set: the ROM is already loaded before
//!   a restore, so `load_state` overwrites only volatile state and then
//!   re-syncs the FastROM speeds via `apply_memsel`.
//! - PPU render caches (tilemap-row / sprite-tile / previous-row): they
//!   rebuild lazily on the next render (their `valid` flags gate every
//!   lookup, so a fresh cache always misses).
//! - Host-side audio (`audio_ring`, the 32 kHz->44.1 kHz resampler, the
//!   scratch vecs) and the input manager: audio resumes cleanly from
//!   the restored APU, input is re-sampled from the host every frame.

use serde::{Deserialize, Serialize};
use spc700::Spc700;
#[cfg(feature = "nec-lle")]
use upd7725::{AluFlags, Upd7725};
use wdc65816::Wdc65816;

/// Payload version for the SNES1 savestate schema. Bumped whenever the
/// shape of [`SnesSaveState`] (or any nested DTO) changes in a way that
/// isn't transparently forward-compatible. Distinct from the envelope's
/// own `envelope_version` (the on-disk wrapper format).
///
/// v2 added `BusState::coprocessor` (the optional DSP-1 uPD7725 blob).
/// v3 generalised that field to [`CoprocessorState`] (an enum that now
/// also carries the HLE backend's state), so the byte layout shifted
/// again. v4 generalised the single optional coprocessor into
/// [`BusState::coprocessors`]: a `Vec<(chip_id, CoprocessorState)>` so a
/// composable cartridge persists every chip in its set, not just one.
/// Because `bincode` is positional, each bump is a hard format break:
/// older states no longer deserialize. That matches the "no legacy
/// fallback" stance `load_state` already takes for missing-magic input.
pub const SNES_SAVESTATE_VERSION: u32 = 4;

// Expected lengths of the big memories, asserted by [`SnesSaveState::validate`].
const WRAM_LEN: usize = 0x2_0000;
const VRAM_LEN: usize = 0x8000;
const OAM_LEN: usize = 0x220;
const CGRAM_LEN: usize = 0x100;
const ARAM_LEN: usize = 0x1_0000;
const DSP_REG_LEN: usize = 128;
/// uPD7725 internal data RAM word count (the DSP-1 coprocessor scratch).
/// A number of the format, so it holds in a build without the `nec-lle`
/// feature too; with it, the crate's own count must agree.
const DSP1_DATA_RAM_LEN: usize = 256;
#[cfg(feature = "nec-lle")]
const _: () = assert!(DSP1_DATA_RAM_LEN == upd7725::DATA_RAM_WORDS);

// -- CPU snapshots ----------------------------------------------------

/// Snapshot of the 65C816 main CPU register file.
///
/// `Default` (all-zero) is derived so that [`Sa1State`], which embeds a
/// second 65C816 for the SA-1 coprocessor, can keep its own `Default`.
/// The zero default never reaches a live CPU: the SA-1 seeds its CPU from
/// [`Wdc65816::new`] and only ever overwrites it from a *real* captured
/// snapshot on restore.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Cpu65816State {
    pub c: u16,
    pub x: u16,
    pub y: u16,
    pub sp: u16,
    pub pc: u16,
    pub pbr: u8,
    pub dbr: u8,
    pub d: u16,
    pub p: u8,
    pub emulation: bool,
    pub cycles_executed: u64,
    pub nmi_pending: bool,
    pub irq_pending: bool,
    pub nmi_edge_detected: bool,
    pub waiting: bool,
    pub stopped: bool,
}

impl Cpu65816State {
    /// Capture the live 65C816 register file. The CPU crate stays free
    /// of serde, we read its public fields here instead.
    pub fn capture(cpu: &Wdc65816) -> Self {
        Self {
            c: cpu.c,
            x: cpu.x,
            y: cpu.y,
            sp: cpu.sp,
            pc: cpu.pc,
            pbr: cpu.pbr,
            dbr: cpu.dbr,
            d: cpu.d,
            p: cpu.p,
            emulation: cpu.emulation,
            cycles_executed: cpu.cycles_executed,
            nmi_pending: cpu.nmi_pending,
            irq_pending: cpu.irq_pending,
            nmi_edge_detected: cpu.nmi_edge_detected,
            waiting: cpu.waiting,
            stopped: cpu.stopped,
        }
    }

    /// Overwrite the live 65C816 registers from this snapshot.
    pub fn restore(&self, cpu: &mut Wdc65816) {
        cpu.c = self.c;
        cpu.x = self.x;
        cpu.y = self.y;
        cpu.sp = self.sp;
        cpu.pc = self.pc;
        cpu.pbr = self.pbr;
        cpu.dbr = self.dbr;
        cpu.d = self.d;
        cpu.p = self.p;
        cpu.emulation = self.emulation;
        cpu.cycles_executed = self.cycles_executed;
        cpu.nmi_pending = self.nmi_pending;
        cpu.irq_pending = self.irq_pending;
        cpu.nmi_edge_detected = self.nmi_edge_detected;
        cpu.waiting = self.waiting;
        cpu.stopped = self.stopped;
    }
}

/// Snapshot of the SPC700 audio CPU register file.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Spc700State {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub sp: u8,
    pub pc: u16,
    pub psw: u8,
    pub cycles_executed: u64,
}

impl Spc700State {
    /// Capture the live SPC700 register file (public fields; the crate
    /// stays serde-free).
    pub fn capture(spc: &Spc700) -> Self {
        Self {
            a: spc.a,
            x: spc.x,
            y: spc.y,
            sp: spc.sp,
            pc: spc.pc,
            psw: spc.psw,
            cycles_executed: spc.cycles_executed,
        }
    }

    /// Overwrite the live SPC700 registers from this snapshot.
    pub fn restore(&self, spc: &mut Spc700) {
        spc.a = self.a;
        spc.x = self.x;
        spc.y = self.y;
        spc.sp = self.sp;
        spc.pc = self.pc;
        spc.psw = self.psw;
        spc.cycles_executed = self.cycles_executed;
    }
}

// -- PPU snapshot -----------------------------------------------------

/// Snapshot of the picture processing unit: the full register file
/// plus VRAM/OAM/CGRAM. Render caches are intentionally excluded, they
/// rebuild lazily from VRAM on the next render.
///
/// `window_enable` is stored as a packed nibble per layer (bit0
/// `w1_enable`, bit1 `w1_invert`, bit2 `w2_enable`, bit3 `w2_invert`)
/// so the DTO doesn't need to mirror the `WindowEnable` leaf struct.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PpuState {
    pub vram: Vec<u16>,
    pub oam: Vec<u8>,
    pub cgram: Vec<u16>,

    pub display_disable: bool,
    pub display_brightness: u8,
    pub obj_base_size: u8,
    pub obj_name_select: u8,
    pub obj_tiledata_addr: u32,
    pub oam_baseaddr: u16,
    pub oam_addr: u16,
    pub oam_priority: bool,
    pub oam_iaddr: u16,
    pub oam_latchdata: u8,
    pub oam_first_sprite: u8,
    pub bgmode: u8,
    pub bg3_priority: bool,
    pub bg_tile_size: [bool; 4],
    pub mosaic_size: u8,
    pub bg_mosaic_enable: [bool; 4],
    pub mosaic_vcounter: u8,
    pub bg_screen_addr: [u32; 4],
    pub bg_screen_size: [u8; 4],
    pub bg_tiledata_addr: [u32; 4],
    pub bg_hoffset: [u16; 4],
    pub bg_voffset: [u16; 4],
    pub bgofs_ppu1_latch: u8,
    pub bgofs_ppu2_latch: u8,
    pub mode7_hoffset: u16,
    pub mode7_voffset: u16,
    pub vmain_byte: u8,
    pub vram_incmode: bool,
    pub vram_mapping: u8,
    pub vram_incsize: u16,
    pub vram_addr: u16,
    pub vram_readbuffer: u16,
    pub mode7_repeat: u8,
    pub mode7_vflip: bool,
    pub mode7_hflip: bool,
    pub m7a: u16,
    pub m7b: u16,
    pub m7c: u16,
    pub m7d: u16,
    pub m7x: u16,
    pub m7y: u16,
    pub mode7_latchdata: u8,
    pub cgram_addr: u16,
    pub cgram_iaddr: u16,
    pub cgram_latchdata: u8,
    pub window_enable: [u8; 6],
    pub window_one_left: u8,
    pub window_one_right: u8,
    pub window_two_left: u8,
    pub window_two_right: u8,
    pub window_combine: [u8; 6],
    pub bg_main_enable: [bool; 4],
    pub obj_main_enable: bool,
    pub bg_sub_enable: [bool; 4],
    pub obj_sub_enable: bool,
    pub bg_main_window_disable: [bool; 4],
    pub obj_main_window_disable: bool,
    pub bg_sub_window_disable: [bool; 4],
    pub obj_sub_window_disable: bool,
    pub col_main_mask: u8,
    pub col_sub_mask: u8,
    pub addsub_mode: bool,
    pub direct_color: bool,
    pub color_math_subtract: bool,
    pub color_math_halve: bool,
    pub color_math_back_enable: bool,
    pub color_math_oam_enable: bool,
    pub color_math_bg_enable: [bool; 4],
    pub fixed_color: u16,
    pub mode7_extbg: bool,
    pub pseudo_hires: bool,
    pub overscan: bool,
    pub obj_interlace: bool,
    pub interlace: bool,
    pub field: bool,
    pub ppu1_mdr: u8,
    pub ppu2_mdr: u8,
    pub sprite_time_over: bool,
    pub sprite_range_over: bool,
}

// -- APU snapshot -----------------------------------------------------

/// Snapshot of one S-DSP voice: its sample ring, where it is in it, its
/// block, the key-on countdown and its envelope. `stage` is the
/// envelope's stage (0 release, 1 attack, 2 decay, 3 sustain).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct VoiceState {
    pub samples: [i32; 12],
    pub ring_head: i32,
    pub position: i32,
    pub block_address: i32,
    pub block_offset: i32,
    pub key_on_countdown: i32,
    pub stage: u8,
    pub level: i32,
    pub envx_out: i32,
    pub level_unclamped: i32,
}

/// Snapshot of the S-DSP: register file + every per-sample pipeline
/// temporary, so a restore resumes synthesis mid-sample bit-for-bit.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DspState {
    pub regs: Vec<u8>,
    pub addr: u8,
    pub voices: [VoiceState; 8],
    pub key_sample: bool,
    pub key_on_mask: i32,
    pub key_on_pending: i32,
    pub noise_lfsr: i32,
    pub rate_counter: i32,
    pub echo_history: [[i32; 8]; 2],
    pub history_head: i32,
    pub echo_offset: i32,
    pub echo_length: i32,
    pub endx_shadow: i32,
    pub envx_shadow: i32,
    pub outx_shadow: i32,
    pub pitch_mod_mask: i32,
    pub noise_mask: i32,
    pub echo_mask: i32,
    pub directory_page: i32,
    pub key_off_mask: i32,
    pub next_block: i32,
    pub envelope_control: i32,
    pub block_header: i32,
    pub block_byte: i32,
    pub source_number: i32,
    pub echo_page: i32,
    pub echo_write_flags: i32,
    pub directory_entry: i32,
    pub pitch: i32,
    pub voice_output: i32,
    pub looped_mask: i32,
    pub echo_pointer: i32,
    pub main_sum: [i32; 2],
    pub echo_sum: [i32; 2],
    pub echo_return: [i32; 2],
}

/// Snapshot of one SPC700 hardware timer.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ApuTimerState {
    pub enabled: bool,
    pub target: u8,
    pub prescaler: u32,
    pub divisor: u32,
    pub stage: u16,
    pub output: u8,
}

/// Snapshot of a [`retro_core::IntegerRateConverter`] anchor. `num`/`den`
/// are re-derived from the console region on restore, so only the
/// drifting state (anchor cycle, cumulative samples, fractional
/// remainder) is persisted.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ConverterState {
    pub reference_cycles: u64,
    pub samples_at_reference: u64,
    pub remainder: u64,
}

/// Snapshot of the whole APU (SPC700 + ARAM + ports + timers + S-DSP +
/// the two drift-free rate converters). The host audio ring is *not*
/// captured, it refills from the restored DSP.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ApuSaveState {
    pub spc700: Spc700State,
    pub aram: Vec<u8>,
    pub ports_cpu_to_apu: [u8; 4],
    pub ports_apu_to_cpu: [u8; 4],
    pub aux: [u8; 2],
    pub timer0: ApuTimerState,
    pub timer1: ApuTimerState,
    pub timer2: ApuTimerState,
    pub dsp: DspState,
    pub control: u8,
    pub spc_sync: ConverterState,
    pub dsp_clock: ConverterState,
    pub apu_cycles: u64,
    pub dsp_samples_emitted: u64,
}

// -- Coprocessor (DSP-1 uPD7725) snapshot -----------------------------

/// Snapshot of the NEC uPD7725 core inside a DSP-1 cartridge.
///
/// Mirrors the volatile architectural state of the `upd7725` crate's core
/// (built with the `nec-lle` feature; the DTO is part of the savestate
/// format and exists in every build, only `capture` / `restore` need the
/// core). The firmware ROMs (`program` / `data_rom`) are deliberately
/// omitted: like the cartridge ROM, they are reloaded from the staged
/// firmware before a restore, never carried in the savestate. The two
/// six-bit ALU flag registers are packed to their bits so the DTO stays
/// flat.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Upd7725State {
    pub stack: [u16; 16],
    pub pc: u16,
    pub rp: u16,
    pub dp: u16,
    pub sp: u8,
    pub k: u16,
    pub l: u16,
    pub m: u16,
    pub n: u16,
    pub a: u16,
    pub b: u16,
    pub flaga: u16,
    pub flagb: u16,
    pub tr: u16,
    pub trb: u16,
    pub sr: u16,
    pub dr: u16,
    pub si: u16,
    pub so: u16,
    pub data_ram: Vec<u16>,
    pub cycles: u64,
}

impl Upd7725State {
    /// A core at power-on with a zeroed data RAM: the shape a savestate
    /// carries, for tests that build one without the core itself.
    #[cfg(test)]
    pub(crate) fn blank() -> Self {
        Self {
            stack: [0; 16],
            pc: 0,
            rp: 0,
            dp: 0,
            sp: 0,
            k: 0,
            l: 0,
            m: 0,
            n: 0,
            a: 0,
            b: 0,
            flaga: 0,
            flagb: 0,
            tr: 0,
            trb: 0,
            sr: 0,
            dr: 0,
            si: 0,
            so: 0,
            data_ram: vec![0; DSP1_DATA_RAM_LEN],
            cycles: 0,
        }
    }

    /// Capture the live uPD7725 register file + data RAM (the crate stays
    /// serde-free; we read its public fields here).
    #[cfg(feature = "nec-lle")]
    pub fn capture(core: &Upd7725) -> Self {
        Self {
            stack: core.call_stack,
            pc: core.program_counter,
            rp: core.rom_pointer,
            dp: core.ram_pointer,
            sp: core.stack_pointer,
            k: core.multiplicand,
            l: core.multiplier,
            m: core.product_high,
            n: core.product_low,
            a: core.accumulators[0],
            b: core.accumulators[1],
            flaga: core.flags[0].to_bits(),
            flagb: core.flags[1].to_bits(),
            tr: core.temporary,
            trb: core.temporary_b,
            sr: core.status,
            dr: core.data_port,
            si: core.serial_in,
            so: core.serial_out,
            data_ram: core.data_ram.to_vec(),
            cycles: core.clocks,
        }
    }

    /// Overwrite the live uPD7725 registers + data RAM from this snapshot.
    /// The firmware ROM already loaded in `core` is left untouched.
    #[cfg(feature = "nec-lle")]
    pub fn restore(&self, core: &mut Upd7725) {
        core.call_stack = self.stack;
        core.program_counter = self.pc;
        core.rom_pointer = self.rp;
        core.ram_pointer = self.dp;
        core.stack_pointer = self.sp;
        core.multiplicand = self.k;
        core.multiplier = self.l;
        core.product_high = self.m;
        core.product_low = self.n;
        core.accumulators = [self.a, self.b];
        core.flags = [
            AluFlags::from_bits(self.flaga),
            AluFlags::from_bits(self.flagb),
        ];
        core.temporary = self.tr;
        core.temporary_b = self.trb;
        core.status = self.sr;
        core.data_port = self.dr;
        core.serial_in = self.si;
        core.serial_out = self.so;
        // `data_ram` is a fixed [u16; 256]; copy the overlap (the length
        // is validated by `SnesSaveState::validate` before any restore).
        let n = core.data_ram.len().min(self.data_ram.len());
        core.data_ram[..n].copy_from_slice(&self.data_ram[..n]);
        core.clocks = self.cycles;
    }
}

/// Snapshot of a DSP-1 coprocessor cartridge's live chip state: the
/// uPD7725 core, its lazy-catch-up converter anchor, and the catch-up
/// cycle counter. Mirrors how the APU persists its SPC700 + converters.
/// Absent from [`BusState::coprocessors`] for plain carts or the idle stub.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Dsp1State {
    pub core: Upd7725State,
    pub sync: ConverterState,
    pub dsp_cycles: u64,
}

/// Snapshot of the firmware-free DSP-1 backend: the command in hand, where
/// the exchange of its bytes stands, and the registers the commands share.
/// Nothing runs between accesses in that backend, so unlike [`Dsp1State`]
/// there is no processor core or clock anchor to keep.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Dsp1HleState {
    /// The last byte taken as a command.
    pub command: u8,
    /// The next byte written to `DR` is a command, not an operand.
    pub awaiting_command: bool,
    /// A command has been taken and none of its operand bytes has arrived.
    pub command_is_fresh: bool,
    /// Operand bytes the command still needs before it runs.
    pub operand_bytes_due: u16,
    /// The operand bytes received so far.
    pub operands: Vec<u8>,
    /// The result bytes not read yet, the next one first. Empty while the
    /// data ROM is being dumped (see `dumping_rom`).
    pub result: Vec<u8>,
    /// The result is the data ROM itself, streamed by the dump command.
    pub dumping_rom: bool,
    /// The next byte of that stream (only while `dumping_rom`).
    pub rom_dump_position: u32,
    /// The registers the commands share, the camera, the three attitude
    /// matrices, the raster line, as raw words.
    pub registers: Vec<i16>,
    /// Reads and writes of `DR`/`SR` since reset.
    pub accesses: u64,
}

/// Snapshot of the S-DD1 mapping controller's persistent state: the four
/// MMC super-bank latches, the two per-DMA-channel enable masks, and the
/// per-channel A-bus source address + byte count reconstructed by spying
/// on `$43xx`. The `dma_shadow` is flattened into parallel arrays so this
/// module stays decoupled from the chip's private `DmaShadow` type. No
/// bitstream state is carried: the decompressor is stateless between
/// transfers (each DMA is atomic in our model), so there is never a
/// mid-decompress savestate to reconstruct.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Sdd1State {
    /// Four MMC super-bank latches, each a byte offset (`super_bank << 20`).
    pub mmc: [u32; 4],
    /// `$4800`: per-channel decompression-enable bitmask.
    pub sdd1_enable: u8,
    /// `$4801`: per-channel transfer-arm bitmask.
    pub xfer_enable: u8,
    /// Per-channel 24-bit A-bus source address (`$43x2-4`).
    pub dma_addr: [u32; 8],
    /// Per-channel transfer byte count (`$43x5-6`; `0` => full 65536).
    pub dma_size: [u16; 8],
}

/// SPC7110 ALU (`$4820-$482F`) snapshot: the operand/result/remainder latches
/// plus the mode and status bytes, exactly as the unit holds them (little-endian
/// byte arrays).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Spc7110AluState {
    /// `$4820-$4827` operand latches.
    pub operands: [u8; 8],
    /// `$4828-$482B` result (product / quotient), little-endian.
    pub result: [u8; 4],
    /// `$482C-$482D` remainder, little-endian.
    pub remainder: [u8; 2],
    /// `$482E` mode (bit 0 = signed).
    pub mode: u8,
    /// `$482F` status (bit 7 = op complete).
    pub status: u8,
}

/// SPC7110 data-port (`$4810-$481A`) snapshot: the pointer/adjust/increment
/// latches, the `$4818` control byte, and the ready / adjust-write latches.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Spc7110DataPortState {
    /// `$4811-$4813` 24-bit pointer, little-endian.
    pub pointer: [u8; 3],
    /// `$4814-$4815` 16-bit adjust, little-endian.
    pub adjust: [u8; 2],
    /// `$4816-$4817` 16-bit increment, little-endian.
    pub increment: [u8; 2],
    /// `$4818` control-bit matrix.
    pub control: u8,
    /// `r481x` ready latch (bits 0-2, armed at `0x07`).
    pub ready: u8,
    /// `$4814` written since the last `$4818` write.
    pub latch_4814: bool,
    /// `$4815` written since the last `$4818` write.
    pub latch_4815: bool,
}

/// SPC7110 decompressor (`$4800-$480C`) snapshot. Every value the decoder
/// carries between bytes is captured here so a save taken **mid-decompression**
/// resumes bit-for-bit. The cartridge ROM is deliberately *not* part of the
/// snapshot, the live chip re-uses its own `Arc<[u8]>` across a restore.
/// The fixed-size buffers are stored as `Vec` (bincode has no array impl past
/// length 32, and this keeps the DTO uniform); the restore tolerates a wrong
/// length defensively.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Spc7110DecompState {
    /// Active mode (`0/1/2`, or `3` = idle).
    pub mode: u32,
    /// Running DATA-ROM read cursor.
    pub offset: u32,
    /// Output ring buffer (64 bytes) + its cursors and fill level.
    pub buffer: Vec<u8>,
    pub rd: u32,
    pub wr: u32,
    pub len: u32,
    /// The 32 adaptive contexts, split into parallel index / invert arrays.
    pub context_index: Vec<u8>,
    pub context_invert: Vec<u8>,
    /// Range-decoder registers.
    pub val: u8,
    pub input: u8,
    pub span: u8,
    /// Modes 0/1 symbol accumulator + LPS / invert histories.
    pub out: u32,
    pub inverts: u32,
    pub lps: u32,
    /// Bits left in `input` before the next `dataread`.
    pub in_count: i32,
    /// Move-to-front pixel predictor (16 entries; mode 1 uses 4, mode 2 all).
    pub pixelorder: Vec<i32>,
    pub realorder: Vec<i32>,
    /// Mode 2 dual accumulators + its 16-byte bitplane reorder buffer.
    pub out0: u32,
    pub out1: u32,
    pub bitplanebuffer: Vec<u8>,
    pub buffer_index: u32,
}

/// SPC7110 RTC-4513 (`$4840-$4842`) *volatile* command-machine snapshot. The
/// 20-byte data file is persistent (it rides `Coprocessor::battery`), so only
/// the state machine + the three port latches are captured here.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Spc7110RtcState {
    pub r4840: u8,
    pub r4841: u8,
    pub r4842: u8,
    /// Command state: `0` Inactive / `1` ModeSelect / `2` IndexSelect / `3` Write.
    pub state: u8,
    /// Access mode wire value: `0x03` Linear / `0x0c` Indexed.
    pub mode: u8,
    /// Current register index (`0-15`).
    pub index: u8,
}

/// SPC7110 full volatile snapshot: the flat `$4800-$4842` register array, the
/// three resolved data-ROM window offsets (`dx/ex/fx`), and each of the four
/// functional units. Battery storage (game SRAM + the 20-byte RTC file)
/// persists separately via `Coprocessor::battery`; the ROM is never captured.
/// `rtc` is `None` on non-RTC (`$F5`) carts.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Spc7110State {
    /// The `$4800-$4842` register array (`REG_COUNT` bytes).
    pub regs: Vec<u8>,
    pub dx_offset: u32,
    pub ex_offset: u32,
    pub fx_offset: u32,
    pub alu: Spc7110AluState,
    pub dataport: Spc7110DataPortState,
    pub decomp: Spc7110DecompState,
    pub rtc: Option<Spc7110RtcState>,
}

/// SA-1 arithmetic-unit snapshot (`$2250-$2254` operands + the 40-bit result).
/// `mr` is the full 40-bit accumulator/product/quotient; `overflow` is the
/// `$230B` bit-7 MAC flag. Held as scalars (not the raw register bytes) because
/// the unit computes over decoded operands.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Sa1MathState {
    /// MCNT `$2250` bit 1 (`acm`): multiply-accumulate mode.
    pub acm: bool,
    /// MCNT `$2250` bit 0 (`md`): divide mode.
    pub md: bool,
    /// MA `$2251/$2252`: multiplicand / dividend.
    pub ma: u16,
    /// MB `$2253/$2254`: multiplier / divisor.
    pub mb: u16,
    /// MR `$2306-$230A`: 40-bit result (product / quotient+remainder / MAC sum).
    pub mr: u64,
    /// `$230B` bit 7: MAC overflow.
    pub overflow: bool,
}

/// SA-1 H/V timer snapshot (`$2210-$2215` control + the free-running H/V
/// counters + the `$2302-$2305` read latches). The counters are in master
/// clocks (HV) or a masked linear count; `scanlines` is the HV `vcounter`
/// wrap (262 NTSC / 312 PAL). Appended to [`Sa1State`], see its note on the
/// no-version-bump growth rule.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Sa1TimerState {
    /// `status.hcounter`: H counter in master clocks.
    pub hcounter: u16,
    /// `status.vcounter`: V counter.
    pub vcounter: u16,
    /// HV `vcounter` wrap (region-dependent: 262 NTSC / 312 PAL).
    pub scanlines: u16,
    /// `$2210` bit 7 (`hvselb`): `false` = HV, `true` = linear.
    pub hvselb: bool,
    /// `$2210` bit 1 (`ven`): V-compare enable.
    pub ven: bool,
    /// `$2210` bit 0 (`hen`): H-compare enable.
    pub hen: bool,
    /// `$2212/13` HCNT compare target (dots).
    pub hcnt: u16,
    /// `$2214/15` VCNT compare target.
    pub vcnt: u16,
    /// `$2302/03` HCR read latch (dots).
    pub hcr: u16,
    /// `$2304/05` VCR read latch.
    pub vcr: u16,
}

/// SA-1 interrupt-controller snapshot: the SA-1-side and S-CPU-side
/// flag / enable / clear latches, the `$2200` CCNT message bits, the message
/// nibbles, the four register vectors, the S-CPU IRQ line latch, and the
/// burst-loop NMI edge detector. The interrupt line is level-triggered on
/// the SA-1 side (recomputed each burst step from the enable and clear
/// latches) and a poked latch on the S-CPU side (`scpu_irq`).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Sa1IrqState {
    // -- SA-1-side sources (CFR `$2301` flag / CIE `$220A` enable / CIC `$220B` clear) --
    pub sa1_irqfl: bool,
    pub timer_irqfl: bool,
    pub dma_irqfl: bool,
    pub sa1_nmifl: bool,
    pub sa1_irqen: bool,
    pub timer_irqen: bool,
    pub dma_irqen: bool,
    pub sa1_nmien: bool,
    pub sa1_irqcl: bool,
    pub timer_irqcl: bool,
    pub dma_irqcl: bool,
    pub sa1_nmicl: bool,
    /// `$2200` CCNT bit 7 (`sa1_irq`): S-CPU->SA-1 IRQ message latch.
    pub sa1_irq: bool,
    /// `$2200` CCNT bit 4 (`sa1_nmi`): S-CPU->SA-1 NMI message latch.
    pub sa1_nmi: bool,
    /// `$2200` CCNT bits 0-3 (`smeg`): S-CPU->SA-1 message nibble (read in CFR).
    pub smeg: u8,
    /// `$2205/06` CNV: SA-1 NMI vector.
    pub cnv: u16,
    /// `$2207/08` CIV: SA-1 IRQ vector (timer / DMA / message).
    pub civ: u16,
    // -- S-CPU-side (SFR `$2300` flag / SIE `$2201` enable / SIC `$2202` clear) --
    pub cpu_irqfl: bool,
    pub chdma_irqfl: bool,
    pub cpu_irqen: bool,
    pub chdma_irqen: bool,
    pub cpu_irqcl: bool,
    pub chdma_irqcl: bool,
    /// `$2209` SCNT bit 6 (`cpu_ivsw`): S-CPU IRQ vector override (VSPROM).
    pub cpu_ivsw: bool,
    /// `$2209` SCNT bit 4 (`cpu_nvsw`): S-CPU NMI vector override (VSPROM).
    pub cpu_nvsw: bool,
    /// `$2209` SCNT bits 0-3 (`cmeg`): SA-1->S-CPU message nibble (read in SFR).
    pub cmeg: u8,
    /// `$220C/0D` SNV: S-CPU NMI vector override.
    pub snv: u16,
    /// `$220E/0F` SIV: S-CPU IRQ vector override.
    pub siv: u16,
    /// The S-CPU IRQ line: poked true by an armed SA-1
    /// message / CC-DMA IRQ, cleared by SIC once no S-CPU-side flag remains.
    pub scpu_irq: bool,
    /// Burst-loop NMI rising-edge detector (previous `sa1_nmi_line()`).
    pub prev_nmi_line: bool,
}

/// SA-1 DMA-engine snapshot: DCNT (`$2230`) + CDMA (`$2231`) parameters, the
/// 24-bit source/destination addresses and terminal counter (`$2232-$2239`),
/// the BW-RAM bitmap format (`$223F`), the 16-byte bitmap register file
/// (`$2240-$224F`), and the CC-DMA type-2 line counter.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Sa1DmaState {
    /// `$2230` bit 7 (`dmaen`): DMA enable.
    pub dmaen: bool,
    /// `$2230` bit 6 (`dprio`): DMA priority (stored; instantaneous DMA ignores).
    pub dprio: bool,
    /// `$2230` bit 5 (`cden`): character-conversion DMA enable.
    pub cden: bool,
    /// `$2230` bit 4 (`cdsel`): CC type select (`1` = type 1 online / `0` = type 2).
    pub cdsel: bool,
    /// `$2230` bit 2 (`dd`): destination device (`false` I-RAM / `true` BW-RAM).
    pub dd: bool,
    /// `$2230` bits 0-1 (`sd`): source device (`0` ROM / `1` BW-RAM / `2` I-RAM).
    pub sd: u8,
    /// `$2231` bit 7 (`chdend`): end character conversion.
    pub chdend: bool,
    /// `$2231` bits 2-4 (`dmasize`): CC tile size (clamped <= 5).
    pub dmasize: u8,
    /// `$2231` bits 0-1 (`dmacb`): CC colour depth (clamped <= 2).
    pub dmacb: u8,
    /// `$2232-$2234` SDA: 24-bit source address.
    pub dsa: u32,
    /// `$2235-$2237` DDA: 24-bit destination address.
    pub dda: u32,
    /// `$2238/39` DTC: 16-bit terminal counter.
    pub dtc: u16,
    /// `$223F` bit 7 (`bbf`): BW-RAM bitmap format (0 = 4bpp / 1 = 2bpp).
    pub bbf: bool,
    /// `$2240-$224F` BRF: the 16-byte bitmap register file.
    pub brf: [u8; 16],
    /// CC-DMA type-2 line counter (`dma.line`, 0-15).
    pub dma_line: u8,
    /// CC-DMA type-1 armed flag: while set, S-CPU
    /// BW-RAM reads perform on-demand character conversion.
    pub cc1_armed: bool,
}

/// SA-1 variable-length bit reader (`$2258-$225B`) savestate. The stream cursor
/// (`va` byte + `vbit` bit), the field width, and the advance mode.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Sa1VarBitState {
    /// `$2259-$225B` VDA: the 24-bit stream byte address.
    pub va: u32,
    /// The bit cursor within the current byte (0-7).
    pub vbit: u8,
    /// `$2258` bits 0-3 VBD: the field width in bits (1-16; 0 = never written).
    pub vb: u8,
    /// `$2258` bit 7 (`hl`): advance mode (`false` fixed / `true` auto-increment).
    pub hl: bool,
}

/// SA-1 *volatile* snapshot: the Super MMC block selects + projection modes
/// (`$2220-$2223`), the BW-RAM/I-RAM select + protection latches
/// (`$2224-$222A`), the arithmetic unit (`$2250-$2254`), the second 65816 CPU
/// plus its catch-up anchor and CCNT/CRV control latches, the 2 KiB I-RAM, the
/// H/V timer, the interrupt controller, and the DMA engine. BW-RAM is
/// battery-backed and persists separately via `Coprocessor::battery`, and the
/// ROM is immutable, so neither is captured here.
///
/// Fields are *appended* (no `CoprocessorState` version bump); `Sa1Chip::restore`
/// is bincode-tolerant, so a legacy shorter blob simply fails to decode and is
/// dropped, leaving the chip at its reset defaults. The `timer`/`irq`/`dma`/
/// `varbit` sub-DTOs each derive `Default`, so a `Sa1State::default()` stays valid.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Sa1State {
    /// Super MMC block selects for blocks C/D/E/F (`$2220-$2223` bits 0-2).
    pub mmc_bank: [u8; 4],
    /// Super MMC projection mode for blocks C/D/E/F (`$2220-$2223` bit 7).
    pub mmc_mode: [bool; 4],
    /// `$2224` BMAPS: the S-CPU BW-RAM 8 KiB window block (`sbm`).
    pub sbm: u8,
    /// `$2225` BMAP: the SA-1 BW-RAM window block (`cbm`).
    pub cbm: u8,
    /// `$2225` BMAP bit 7 (`sw46`): SA-1 BW-RAM bitmap-mode select.
    pub sw46: bool,
    /// `$2226-$222A` write-enable + protection latches.
    pub protect: [u8; 5],
    /// The arithmetic unit (`$2250-$2254` operands + `$2306-$230B` result).
    pub math: Sa1MathState,
    /// The 2 KiB I-RAM (volatile, not battery-backed).
    pub iram: Vec<u8>,
    /// The SA-1's own 65816 register file.
    pub cpu: Cpu65816State,
    /// Master clocks the SA-1 CPU has consumed (the catch-up anchor).
    pub sa1_master: u64,
    /// `$2203/04` CRV: the SA-1 CPU reset vector.
    pub crv: u16,
    /// `$2200` CCNT bit 5 (`resb`): SA-1 CPU held in reset (asleep).
    pub sa1_resb: bool,
    /// `$2200` CCNT bit 6 (`rdyb`): SA-1 CPU parked (asleep).
    pub sa1_rdyb: bool,
    /// A pending deferred wake (`Some(crv)`) not yet applied to PC.
    pub pending_reset: Option<u16>,
    /// The H/V timer (`$2210-$2215` + `$2302-$2305`).
    pub timer: Sa1TimerState,
    /// The interrupt controller (SA-1 + S-CPU flag/enable/clear + vectors).
    pub irq: Sa1IrqState,
    /// The DMA engine (`$2230-$224F`).
    pub dma: Sa1DmaState,
    /// The variable-length bit reader (`$2258-$225B` cursor + width + mode).
    pub varbit: Sa1VarBitState,
}

/// One of the Super FX plot engine's two pixel-cache lines, for the
/// [`SuperFxState`] snapshot. Mirrors the GSU's `PixelCache` (a flush
/// offset, a per-column pending bitmask, and one colour byte per column).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct SuperFxPixelCacheState {
    /// Game Pak RAM byte offset this cache line flushes to.
    pub offset: u16,
    /// Bitmask of the 8 pending pixel columns.
    pub bitpend: u8,
    /// One colour byte per column.
    pub data: [u8; 8],
}

/// Super FX *volatile* snapshot: the GSU register file (`R0..R15`, `SFR`,
/// the bank/cache/screen registers, `POR`/`CFGR`/`CLSR`), the ROM/RAM
/// buffer latches, the 512-byte instruction cache + per-block valid flags,
/// the two plot pixel caches, the branch-pending `r15_modified` latch, the
/// S-CPU access counter, the GSU catch-up anchor, the data-bus MDR proxy,
/// and the **full Game Pak RAM**. Capturing the RAM makes the savestate the
/// complete machine state (Star Fox's Game Pak RAM is volatile, so a
/// savestate would otherwise lose it); a battery-backed cart *additionally*
/// persists that RAM via [`Coprocessor::battery`] into the `.srm`. The ROM
/// is immutable, so it is never captured.
///
/// The cache buffer/valid arrays, the pixel caches, and the Game Pak RAM are
/// stored as `Vec`s (bincode caps derives at 32-element arrays);
/// [`SuperFxChip::restore`] copies `min(len)` so a short/long blob is
/// tolerated. Fields are *appended*, a legacy shorter blob simply fails to
/// decode and is dropped, leaving the chip at its reset defaults.
///
/// [`Coprocessor::battery`]: retro_core::cartridge::Coprocessor::battery
/// [`SuperFxChip::restore`]: crate::cartridge::SuperFxChip
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct SuperFxState {
    /// General-purpose registers `R0..R15` (`R15` = PC).
    pub r: [u16; 16],
    /// Status flag register (`SFR`).
    pub sfr: u16,
    /// Program bank register.
    pub pbr: u8,
    /// Game Pak ROM bank register.
    pub rombr: u8,
    /// Game Pak RAM bank register (1-bit).
    pub rambr: bool,
    /// Cache base register.
    pub cbr: u16,
    /// Screen base register.
    pub scbr: u8,
    /// Screen mode register (`SCMR`).
    pub scmr: u8,
    /// Colour register.
    pub colr: u8,
    /// Plot option register (`POR`).
    pub por: u8,
    /// Back-up RAM register (battery-backed Game Pak RAM enable).
    pub bramr: bool,
    /// Version code register (reads `$04`).
    pub vcr: u8,
    /// Config register (`CFGR`).
    pub cfgr: u8,
    /// Clock divider selected by `CLSR` (`2` = 10.7 MHz, `1` = 21.4 MHz).
    pub clsr_divider: u8,
    /// Pipeline register (pre-fetched next opcode).
    pub pipeline: u8,
    /// RAM buffer effective address.
    pub ramaddr: u16,
    /// Source register index for the next ALU op.
    pub sreg: u8,
    /// Destination register index for the next ALU op.
    pub dreg: u8,
    /// ROM read latency counter.
    pub romcl: u8,
    /// ROM buffer data register.
    pub romdr: u8,
    /// RAM buffer address register.
    pub ramar: u16,
    /// RAM write latency counter.
    pub ramcl: u8,
    /// RAM buffer data register.
    pub ramdr: u8,
    /// The 512-byte instruction cache buffer.
    pub cache_buffer: Vec<u8>,
    /// Per-block cache valid flags (32 blocks).
    pub cache_valid: Vec<bool>,
    /// The two plot pixel-cache lines.
    pub pixelcache: Vec<SuperFxPixelCacheState>,
    /// GSU clock cycles executed since reset.
    pub cycles_executed: u64,
    /// S-CPU accesses to the chip since reset.
    pub accesses: u64,
    /// GSU catch-up anchor, in the S-CPU master-clock domain (the chip drives
    /// the GSU forward to this cursor on each access). Appended after
    /// `accesses`; an older blob without it fails to decode and is dropped.
    pub gsu_master: u64,
    /// Last byte on the chip's S-CPU data bus, returned as the open-bus value
    /// while the GSU owns Game Pak RAM (`SCMR.RAN`).
    pub last_mdr: u8,
    /// Whether the in-flight instruction wrote `R15` (a taken branch/jump),
    /// which suppresses the post-instruction PC auto-increment. Appended for
    /// mid-instruction restore fidelity.
    pub r15_modified: bool,
    /// The full Game Pak RAM. Captured so the savestate is machine-complete
    /// (volatile RAM survives save/load without the `.srm`); copied `min(len)`
    /// on restore. Appended last for back-compat.
    pub ram: Vec<u8>,
}

/// Cx4 (Hitachi HG51B169) *volatile* snapshot: the whole execution core,
/// the architectural register file (`pc`/`p`/flags/`a`/`acch`/`accl`, the data
/// latches, the 16 GPRs, `mdr`, the 8-deep call stack, `halt`/`cache_page`),
/// the asynchronous cart-bus transfer latches, the S-CPU MMIO latches
/// (DMA/program/speed/IRQ/suspend + the 32 vector bytes), both instruction-cache
/// pages (lock + page number + the 256 words each), and the full 3072-byte
/// internal **data RAM**, so a save
/// taken mid-transform is machine-complete. The chip's rate-converter anchor
/// (`sync`) and cumulative chip-cycle counter (`cx4_cycles` = the core's
/// `cycles`) ride along so the lazy catch-up resumes drift-free.
///
/// The cartridge ROM and the embedded data ROM are immutable (reloaded before a
/// restore), and cart RAM is battery-backed (persists via
/// [`Coprocessor::battery`](retro_core::cartridge::Coprocessor::battery)), so
/// none of the three is captured here. Fields are *appended* (never reordered);
/// the `Vec`-typed cache/data-RAM/vector members restore `min(len)`, so a
/// short/long blob is tolerated rather than panicking.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Cx4State {
    pub pc: u32,
    pub p: u16,
    pub n: bool,
    pub z: bool,
    pub v: bool,
    pub c: bool,
    pub a: u32,
    pub acch: u32,
    pub accl: u32,
    pub busdata: u32,
    pub romdata: u32,
    pub ramdata: u32,
    pub busaddr: u32,
    pub ramaddr: u32,
    pub gpr: [u32; 16],
    pub mdr: u32,
    pub stack: [u32; 8],
    pub halt: bool,
    pub cache_page: u8,
    /// Pending asynchronous cart-bus transfer latches (`$2E`/`$2F` bus ops).
    pub rwbusaddr: u32,
    pub rwbustime: u8,
    pub writebus: bool,
    pub writebusdata: u32,
    /// S-CPU MMIO latches (`$7F40-7F5F`).
    pub dma_source: u32,
    pub dma_length: u16,
    pub dma_target: u32,
    pub program_offset: u32,
    pub page_number: u16,
    pub program_counter: u8,
    pub rom_speed: u8,
    pub ram_speed: u8,
    pub irq_disable: bool,
    pub irq_pending: bool,
    pub dma: bool,
    pub cache_loading: bool,
    pub cache_preload: u8,
    pub suspend: bool,
    pub suspend_cycles: u32,
    /// The 32 vector bytes (`$7F60-7F7F`).
    pub vectors: Vec<u8>,
    /// Per-page instruction-cache lock + program page.
    pub cache_lock: [bool; 2],
    pub cache_page_number: [u16; 2],
    /// Both cache pages' words, concatenated (`2 x 256`).
    pub cache_data: Vec<u16>,
    /// The 3072-byte internal data RAM.
    pub data_ram: Vec<u8>,
    /// The lazy-catch-up rate-converter anchor.
    pub sync: ConverterState,
    /// Cumulative HG51B169 chip cycles (the core's `add_clocks` accumulator).
    pub cx4_cycles: u64,
    /// S-CPU accesses to the chip since reset.
    pub accesses: u64,
}

/// Backend-tagged coprocessor savestate. The cartridge persists whichever
/// variant matches the live backend; on restore the bus hands the blob
/// back to the installed chip, which ignores a variant it can't consume
/// (e.g. an LLE blob restored into an HLE chip, or vice versa). Keeping
/// the tag explicit means a state saved under one backend never silently
/// corrupts the other.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum CoprocessorState {
    /// Low-level emulation: the full uPD7725 core + converter anchor.
    Dsp1Lle(Dsp1State),
    /// High-level emulation: the host-visible command/handshake state.
    Dsp1Hle(Dsp1HleState),
    /// S-DD1 mapping controller: MMC latches, enable masks, and the
    /// per-channel DMA shadow. Appended last so the existing variant
    /// indices (0/1) stay stable and older savestates still decode.
    Sdd1(Sdd1State),
    /// SPC7110: the four functional units (`$4800-$4842`) + resolved dx/ex/fx
    /// window offsets. Appended after S-DD1 so variant indices 0-2 stay
    /// stable and older savestates still decode.
    Spc7110(Spc7110State),
    /// SA-1: the Super MMC mapping state + I-RAM. Appended after SPC7110 so
    /// variant indices 0-3 stay stable and older savestates still decode (no
    /// schema version bump needed).
    Sa1(Sa1State),
    /// Super FX: the GSU register file + ROM/RAM buffers + instruction cache +
    /// plot pixel caches. Appended after SA-1 so variant indices 0-4 stay
    /// stable and older savestates still decode (no schema version bump).
    SuperFx(SuperFxState),
    /// Cx4: the HG51B169 core (registers + stack + MMIO latches), both
    /// instruction-cache pages, and the internal data RAM. Appended after
    /// Super FX so variant indices 0-5 stay stable and older savestates still
    /// decode (no schema version bump).
    Cx4(Cx4State),
}

// -- DMA / system-reg / controller / wmdata snapshots -----------------

/// Snapshot of one DMA/HDMA channel's `$43xx` register file plus the
/// two hidden HDMA flags.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct DmaChannelState {
    pub params: u8,
    pub dest_addr: u8,
    pub source_addr: u16,
    pub source_bank: u8,
    pub das: u16,
    pub indirect_bank: u8,
    pub hdma_addr: u16,
    pub line_counter: u8,
    pub unused: u8,
    pub hdma_completed: bool,
    pub hdma_do_transfer: bool,
}

/// Snapshot of the eight DMA/HDMA channels.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DmaControllerState {
    pub channels: [DmaChannelState; 8],
}

/// Snapshot of the 5A22 system registers (`$4200-$421F`) including the
/// settled multiply/divide results and the latched H/V counters.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SystemRegsState {
    pub nmitimen: u8,
    pub wrio: u8,
    pub wrmpya: u8,
    pub wrmpyb: u8,
    pub wrdivl: u8,
    pub wrdivh: u8,
    pub wrdivb: u8,
    pub htime: u16,
    pub vtime: u16,
    pub mdmaen: u8,
    pub hdmaen: u8,
    pub memsel: u8,
    pub mpy_result: u16,
    pub div_quotient: u16,
    pub math_cycles_remaining: u8,
    pub rdnmi: u8,
    pub timeup: u8,
    pub hvbjoy: u8,
    pub rdio_latch: u8,
    pub joy_data: [u8; 8],
    pub h_latched: u16,
    pub v_latched: u16,
    pub h_toggle: bool,
    pub v_toggle: bool,
    pub counters_latched: bool,
    pub timing_is_pal: bool,
}

/// Snapshot of one standard controller port (held buttons + serial
/// shift register + strobe line).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ControllerState {
    pub buttons: u16,
    pub shift_register: u16,
    pub strobe: bool,
}

/// Snapshot of the WMDATA/WMADD 17-bit WRAM portal pointer.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct WmdataSaveState {
    pub wm_addr: u32,
}

// -- Scheduler snapshot -----------------------------------------------

/// Mirror of [`crate::scheduler::TimingProfile`] for the wire format.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimingState {
    pub master_clock_hz: u32,
    pub cycles_per_scanline: u32,
    pub scanlines_per_frame: u16,
    pub vblank_start_line: u16,
}

/// Mirror of [`crate::scheduler::EventKind`] for the wire format.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum EventKindState {
    HBlankStart,
    RenderPos,
    HdmaInit,
    HdmaTrigger,
    VBlankStart,
    VBlankEnd,
    IrqV { v_target: u16 },
    IrqH { h_target: u16 },
    IrqHV { v: u16, h: u16 },
    ApuCatchup,
    AutoJoypadDone,
    FrameEnd,
}

/// Snapshot of one scheduled event.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ScheduledEventState {
    pub at_cycle: u64,
    pub seq: u64,
    pub kind: EventKindState,
}

/// Snapshot of the H-event scheduler. The `BinaryHeap` is flattened to
/// a `Vec` (order is irrelevant, the heap is rebuilt on load).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SchedulerState {
    pub current_cycle: u64,
    pub next_seq: u64,
    pub timing: TimingState,
    pub events: Vec<ScheduledEventState>,
}

// -- Bus + top-level snapshot -----------------------------------------

/// Snapshot of the main address-space owner: WRAM, the open-bus MDR,
/// the MEMSEL FastROM bit, the cached raster snapshot, the deferred
/// pending flags, the APU catch-up cycle, every owned subsystem, and
/// the battery SRAM pulled from the cartridge.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BusState {
    pub wram: Vec<u8>,
    pub mdr: u8,
    pub memsel_fast: bool,
    pub cached_h_dot: u16,
    pub cached_v_scanline: u16,
    pub pending_general_dma: u8,
    pub pending_irq_reschedule: bool,
    pub pending_nmi: bool,
    pub apu_cycle: u64,
    pub ppu: PpuState,
    pub apu: ApuSaveState,
    pub dma: DmaControllerState,
    pub system_regs: SystemRegsState,
    pub controllers: [ControllerState; 2],
    pub wmdata: WmdataSaveState,
    /// Battery SRAM bytes copied out of the cartridge. Empty when the
    /// cartridge has no SRAM.
    pub sram: Vec<u8>,
    /// Coprocessor chip state, keyed by `chip_id`. Each entry is
    /// backend-tagged (LLE uPD7725 core + converter anchor, or the HLE
    /// command/handshake state). Empty for plain cartridges and the idle
    /// stub; a composable cartridge with two chips persists two entries.
    pub coprocessors: Vec<(u8, CoprocessorState)>,
}

/// The complete SNES1 savestate payload wrapped by the shared
/// [`retro_core::savestate::SaveStateEnvelope`].
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SnesSaveState {
    pub version: u32,
    pub cpu: Cpu65816State,
    pub bus: BusState,
    pub scheduler: SchedulerState,
    pub master_cycles: u64,
    pub frame_base_cycle: u64,
    pub frame_count: u64,
    pub timing: TimingState,
    /// Debugger-focused CPU id (0 = 5A22, 1 = SPC700, 2 = the cartridge's
    /// CPU on an SA-1, Super FX or Cx4 cart). Stored narrow because the
    /// valid values are 0 to 2.
    pub focused_cpu: u8,
    pub audio_enabled: bool,
}

impl SnesSaveState {
    /// Bincode size of the payload (pre-compression), for tests and
    /// telemetry.
    pub fn size(&self) -> usize {
        bincode::serialized_size(self).unwrap_or(0) as usize
    }

    /// Reject payloads from a newer schema or with the wrong big-memory
    /// lengths. Called by `load_state` before any field is restored.
    ///
    /// The version check rejects anything newer than the current schema;
    /// the size checks then validate the big memories, including the
    /// DSP-1 data RAM when a coprocessor blob is present.
    pub fn validate(&self) -> Result<(), String> {
        if self.version > SNES_SAVESTATE_VERSION {
            return Err(format!(
                "SNES save state version {} is newer than supported version {}",
                self.version, SNES_SAVESTATE_VERSION
            ));
        }
        check_len("WRAM", self.bus.wram.len(), WRAM_LEN)?;
        check_len("VRAM", self.bus.ppu.vram.len(), VRAM_LEN)?;
        check_len("OAM", self.bus.ppu.oam.len(), OAM_LEN)?;
        check_len("CGRAM", self.bus.ppu.cgram.len(), CGRAM_LEN)?;
        check_len("ARAM", self.bus.apu.aram.len(), ARAM_LEN)?;
        check_len("DSP regs", self.bus.apu.dsp.regs.len(), DSP_REG_LEN)?;
        // Only the LLE variant carries the fixed-size uPD7725 data RAM;
        // the HLE variant has variable-length protocol buffers with no
        // length invariant to assert here. Every chip in the set is checked.
        for (_id, state) in &self.bus.coprocessors {
            if let CoprocessorState::Dsp1Lle(dsp) = state {
                check_len("DSP-1 data RAM", dsp.core.data_ram.len(), DSP1_DATA_RAM_LEN)?;
            }
        }
        Ok(())
    }
}

fn check_len(name: &str, got: usize, want: usize) -> Result<(), String> {
    if got == want {
        Ok(())
    } else {
        Err(format!("Invalid {name} size: {got} (expected {want})"))
    }
}

// -- WindowEnable packing helpers -------------------------------------

/// Pack a [`crate::ppu::WindowEnable`] into a nibble for [`PpuState`].
pub fn pack_window_enable(we: &crate::ppu::WindowEnable) -> u8 {
    (we.w1_enable as u8)
        | ((we.w1_invert as u8) << 1)
        | ((we.w2_enable as u8) << 2)
        | ((we.w2_invert as u8) << 3)
}

/// Unpack a [`PpuState`] window nibble back into a
/// [`crate::ppu::WindowEnable`].
pub fn unpack_window_enable(bits: u8) -> crate::ppu::WindowEnable {
    crate::ppu::WindowEnable {
        w1_enable: bits & 0x01 != 0,
        w1_invert: bits & 0x02 != 0,
        w2_enable: bits & 0x04 != 0,
        w2_invert: bits & 0x08 != 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_enable_pack_round_trips_all_16_combinations() {
        for bits in 0u8..16 {
            let we = unpack_window_enable(bits);
            assert_eq!(pack_window_enable(&we), bits);
        }
    }

    #[test]
    fn validate_rejects_future_version() {
        let mut state = sample_state();
        state.version = SNES_SAVESTATE_VERSION + 1;
        assert!(state.validate().is_err());
    }

    #[test]
    fn validate_rejects_wrong_wram_len() {
        let mut state = sample_state();
        state.bus.wram.truncate(10);
        let err = state.validate().unwrap_err();
        assert!(err.contains("WRAM"), "unexpected error: {err}");
    }

    #[test]
    fn validate_accepts_well_formed_state() {
        assert!(sample_state().validate().is_ok());
    }

    #[cfg(feature = "nec-lle")]
    #[test]
    fn upd7725_state_captures_and_restores_registers_and_ram() {
        let mut core = Upd7725::new();
        core.program_counter = 0x123;
        core.rom_pointer = 0x2AA;
        core.ram_pointer = 0x55;
        core.stack_pointer = 0x0A;
        core.accumulators = [0xBEEF, 0x1234];
        core.data_port = 0x00AA;
        core.status = 0x8000;
        core.flags = [AluFlags::from_bits(0x2A), AluFlags::from_bits(0x15)];
        core.data_ram[0] = 0xCAFE;
        core.data_ram[255] = 0x0FF0;
        core.clocks = 9_999;

        let snap = Upd7725State::capture(&core);
        assert_eq!(snap.data_ram.len(), DSP1_DATA_RAM_LEN);

        // Restore into a pristine core and confirm every field round-trips.
        let mut fresh = Upd7725::new();
        snap.restore(&mut fresh);
        assert_eq!(fresh.program_counter, 0x123);
        assert_eq!(fresh.rom_pointer, 0x2AA);
        assert_eq!(fresh.ram_pointer, 0x55);
        assert_eq!(fresh.stack_pointer, 0x0A);
        assert_eq!(fresh.accumulators, [0xBEEF, 0x1234]);
        assert_eq!(fresh.data_port, 0x00AA);
        assert_eq!(fresh.status, 0x8000);
        assert_eq!(fresh.flags[0], AluFlags::from_bits(0x2A));
        assert_eq!(fresh.flags[1], AluFlags::from_bits(0x15));
        assert_eq!(fresh.data_ram[0], 0xCAFE);
        assert_eq!(fresh.data_ram[255], 0x0FF0);
        assert_eq!(fresh.clocks, 9_999);
    }

    #[test]
    fn validate_accepts_well_formed_coprocessor() {
        let mut state = sample_state();
        state.bus.coprocessors = vec![(
            1,
            CoprocessorState::Dsp1Lle(Dsp1State {
                core: Upd7725State::blank(),
                sync: ConverterState::default(),
                dsp_cycles: 42,
            }),
        )];
        assert!(state.validate().is_ok());
    }

    #[test]
    fn validate_rejects_wrong_dsp_data_ram_len() {
        let mut state = sample_state();
        let mut dsp_core = Upd7725State::blank();
        dsp_core.data_ram.truncate(10); // not 256 words
        state.bus.coprocessors = vec![(
            1,
            CoprocessorState::Dsp1Lle(Dsp1State {
                core: dsp_core,
                sync: ConverterState::default(),
                dsp_cycles: 0,
            }),
        )];
        let err = state.validate().unwrap_err();
        assert!(err.contains("DSP-1 data RAM"), "unexpected error: {err}");
    }

    #[test]
    fn validate_accepts_hle_coprocessor_without_data_ram_check() {
        // The HLE variant has no fixed-size data RAM, so validate() must
        // not try to length-check it (it would otherwise reject every
        // HLE save). A populated protocol buffer is well-formed.
        let mut state = sample_state();
        state.bus.coprocessors = vec![(
            1,
            CoprocessorState::Dsp1Hle(Dsp1HleState {
                command: 0x10,
                awaiting_command: false,
                command_is_fresh: false,
                operand_bytes_due: 0,
                operands: vec![0x12, 0x34],
                result: vec![0x56],
                dumping_rom: false,
                rom_dump_position: 0,
                registers: vec![1, -2, 3],
                accesses: 7,
            }),
        )];
        assert!(state.validate().is_ok());
    }

    /// Minimal well-formed payload with correctly-sized memories so the
    /// `validate` size checks pass.
    fn sample_state() -> SnesSaveState {
        SnesSaveState {
            version: SNES_SAVESTATE_VERSION,
            cpu: Cpu65816State::capture(&Wdc65816::new()),
            bus: BusState {
                wram: vec![0u8; WRAM_LEN],
                mdr: 0,
                memsel_fast: false,
                cached_h_dot: 0,
                cached_v_scanline: 0,
                pending_general_dma: 0,
                pending_irq_reschedule: false,
                pending_nmi: false,
                apu_cycle: 0,
                ppu: PpuState {
                    vram: vec![0u16; VRAM_LEN],
                    oam: vec![0u8; OAM_LEN],
                    cgram: vec![0u16; CGRAM_LEN],
                    display_disable: true,
                    display_brightness: 0,
                    obj_base_size: 0,
                    obj_name_select: 0,
                    obj_tiledata_addr: 0,
                    oam_baseaddr: 0,
                    oam_addr: 0,
                    oam_priority: false,
                    oam_iaddr: 0,
                    oam_latchdata: 0,
                    oam_first_sprite: 0,
                    bgmode: 0,
                    bg3_priority: false,
                    bg_tile_size: [false; 4],
                    mosaic_size: 1,
                    bg_mosaic_enable: [false; 4],
                    mosaic_vcounter: 0,
                    bg_screen_addr: [0; 4],
                    bg_screen_size: [0; 4],
                    bg_tiledata_addr: [0; 4],
                    bg_hoffset: [0; 4],
                    bg_voffset: [0; 4],
                    bgofs_ppu1_latch: 0,
                    bgofs_ppu2_latch: 0,
                    mode7_hoffset: 0,
                    mode7_voffset: 0,
                    vmain_byte: 0,
                    vram_incmode: false,
                    vram_mapping: 0,
                    vram_incsize: 1,
                    vram_addr: 0,
                    vram_readbuffer: 0,
                    mode7_repeat: 0,
                    mode7_vflip: false,
                    mode7_hflip: false,
                    m7a: 0,
                    m7b: 0,
                    m7c: 0,
                    m7d: 0,
                    m7x: 0,
                    m7y: 0,
                    mode7_latchdata: 0,
                    cgram_addr: 0,
                    cgram_iaddr: 0,
                    cgram_latchdata: 0,
                    window_enable: [0; 6],
                    window_one_left: 0,
                    window_one_right: 0,
                    window_two_left: 0,
                    window_two_right: 0,
                    window_combine: [0; 6],
                    bg_main_enable: [false; 4],
                    obj_main_enable: false,
                    bg_sub_enable: [false; 4],
                    obj_sub_enable: false,
                    bg_main_window_disable: [false; 4],
                    obj_main_window_disable: false,
                    bg_sub_window_disable: [false; 4],
                    obj_sub_window_disable: false,
                    col_main_mask: 0,
                    col_sub_mask: 0,
                    addsub_mode: false,
                    direct_color: false,
                    color_math_subtract: false,
                    color_math_halve: false,
                    color_math_back_enable: false,
                    color_math_oam_enable: false,
                    color_math_bg_enable: [false; 4],
                    fixed_color: 0,
                    mode7_extbg: false,
                    pseudo_hires: false,
                    overscan: false,
                    obj_interlace: false,
                    interlace: false,
                    field: false,
                    ppu1_mdr: 0,
                    ppu2_mdr: 0,
                    sprite_time_over: false,
                    sprite_range_over: false,
                },
                apu: ApuSaveState {
                    spc700: Spc700State::capture(&Spc700::new()),
                    aram: vec![0u8; ARAM_LEN],
                    ports_cpu_to_apu: [0; 4],
                    ports_apu_to_cpu: [0; 4],
                    aux: [0; 2],
                    timer0: sample_timer(),
                    timer1: sample_timer(),
                    timer2: sample_timer(),
                    dsp: DspState {
                        regs: vec![0u8; DSP_REG_LEN],
                        addr: 0,
                        voices: core::array::from_fn(|_| sample_voice()),
                        key_sample: true,
                        key_on_mask: 0,
                        key_on_pending: 0,
                        noise_lfsr: 0x4000,
                        rate_counter: 0,
                        echo_history: [[0; 8]; 2],
                        history_head: 0,
                        echo_offset: 0,
                        echo_length: 0,
                        endx_shadow: 0,
                        envx_shadow: 0,
                        outx_shadow: 0,
                        pitch_mod_mask: 0,
                        noise_mask: 0,
                        echo_mask: 0,
                        directory_page: 0,
                        key_off_mask: 0,
                        next_block: 0,
                        envelope_control: 0,
                        block_header: 0,
                        block_byte: 0,
                        source_number: 0,
                        echo_page: 0,
                        echo_write_flags: 0,
                        directory_entry: 0,
                        pitch: 0,
                        voice_output: 0,
                        looped_mask: 0,
                        echo_pointer: 0,
                        main_sum: [0; 2],
                        echo_sum: [0; 2],
                        echo_return: [0; 2],
                    },
                    control: 0x80,
                    spc_sync: ConverterState::default(),
                    dsp_clock: ConverterState::default(),
                    apu_cycles: 0,
                    dsp_samples_emitted: 0,
                },
                dma: DmaControllerState {
                    channels: core::array::from_fn(|_| DmaChannelState::default()),
                },
                system_regs: SystemRegsState {
                    nmitimen: 0,
                    wrio: 0,
                    wrmpya: 0,
                    wrmpyb: 0,
                    wrdivl: 0,
                    wrdivh: 0,
                    wrdivb: 0,
                    htime: 0,
                    vtime: 0,
                    mdmaen: 0,
                    hdmaen: 0,
                    memsel: 0,
                    mpy_result: 0,
                    div_quotient: 0,
                    math_cycles_remaining: 0,
                    rdnmi: 0,
                    timeup: 0,
                    hvbjoy: 0,
                    rdio_latch: 0,
                    joy_data: [0; 8],
                    h_latched: 0,
                    v_latched: 0,
                    h_toggle: false,
                    v_toggle: false,
                    counters_latched: false,
                    timing_is_pal: false,
                },
                controllers: [ControllerState::default(), ControllerState::default()],
                wmdata: WmdataSaveState::default(),
                sram: Vec::new(),
                coprocessors: Vec::new(),
            },
            scheduler: SchedulerState {
                current_cycle: 0,
                next_seq: 0,
                timing: TimingState {
                    master_clock_hz: 21_477_270,
                    cycles_per_scanline: 1364,
                    scanlines_per_frame: 262,
                    vblank_start_line: 225,
                },
                events: Vec::new(),
            },
            master_cycles: 0,
            frame_base_cycle: 0,
            frame_count: 0,
            timing: TimingState {
                master_clock_hz: 21_477_270,
                cycles_per_scanline: 1364,
                scanlines_per_frame: 262,
                vblank_start_line: 225,
            },
            focused_cpu: 0,
            audio_enabled: false,
        }
    }

    fn sample_timer() -> ApuTimerState {
        ApuTimerState {
            enabled: false,
            target: 0,
            prescaler: 0,
            divisor: 128,
            stage: 0,
            output: 0,
        }
    }

    fn sample_voice() -> VoiceState {
        VoiceState {
            samples: [0; 12],
            ring_head: 0,
            position: 0,
            block_address: 0,
            block_offset: 1,
            key_on_countdown: 0,
            stage: 0,
            level: 0,
            envx_out: 0,
            level_unclamped: 0,
        }
    }
}
