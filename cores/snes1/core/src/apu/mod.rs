//! Audio Processing Unit: SPC700 CPU + S-DSP + 64 KiB of ARAM.
//!
//! The APU is a self-contained sound computer that runs asynchronously
//! to the 5A22 main CPU. It communicates with the main bus through
//! four shared mailbox ports (`$2140-$2143` on the 5A22 side, `$F4-$F7`
//! on the SPC700 side). The SPC700 boots from a fixed 64-byte IPL ROM,
//! drives an S-DSP that emits stereo samples at 32 kHz, and has three
//! hardware timers the audio driver polls for pacing.
//!
//! ## Synchronisation model
//!
//! The main system advances in master clocks. The APU is caught up
//! *lazily*: whenever the main CPU touches a mailbox port (and once
//! per frame), [`Apu::catch_up_to`] runs the SPC700 forward to the SPC
//! cycle that corresponds to the current master cycle. Two
//! [`IntegerRateConverter`]s, both keyed on the same master-cycle
//! counter, drive this:
//!
//! - `spc_sync` (1.024 MHz / master clock) gives the target SPC cycle.
//! - `dsp_clock` (32 kHz / master clock) gives the target cumulative
//!   sample count.
//!
//! Because both are exact integer functions of the master cycle, the
//! SPC clock and the sample clock cannot drift against each other, by
//! construction, over any length of emulation.
//!
//! S-DSP sample generation is *interleaved* with the SPC700 step loop,
//! not batched after it: one 32 kHz sample is emitted every 32 SPC
//! cycles (the exact hardware ratio), so each DSP register write, a
//! voice key-on above all, is observed at the sample it lands on. The
//! window's total is still pinned to `dsp_clock`'s drift-free target,
//! so interleaving changes only *when* within the window a sample is
//! produced; emitting samples as the SPC clock advances is what lets a
//! music driver's rapid key-on / key-off pulses survive instead of
//! collapsing to the window's final register state.
//!
//! Each due sample is synthesised by the S-DSP ([`dsp`]): eight BRR
//! voices, per-voice envelopes, the echo unit (8-tap FIR + feedback +
//! ARAM echo buffer), and the stereo mixer, run as the chip's 32-clock
//! pipeline. The resulting 32 kHz stereo `i16` samples are pushed into
//! [`Apu::audio_ring`]. On the play path the emulator drains this ring
//! once per frame and resamples it to the 44.1 kHz host rate (see
//! `SnesEmulator::drain_apu_to_host_audio`); when audio is disabled the
//! ring instead bounds-overflows its oldest samples (benign).

mod bus;
mod debug;
mod dsp;
mod ipl;
mod timers;

use retro_core::audio::{
    AudioRingBuffer, IntegerRateConverter, SampleKind, ScopeRing, SoundEvent, SoundLog,
    SoundLogContents, SoundLogEntry, StemChannel, StemLayout, StemRecorder, StemSet, StemSum,
    StemTake,
};
use retro_core::cpu::Addressable;
use spc700::Spc700;

use crate::debugger::TraceEntryBinarySpc700;
use bus::{ApuBus, ApuState, DspLog};

pub use bus::ARAM_SIZE;
pub use dsp::{
    decode_sample, directory, loop_passes, BrrEnd, BrrSample, DirectoryEntry, LoopPasses,
    LOOP_VALUES_CAP,
};

/// SPC700 master clock in Hz (~1.024 MHz).
const SPC_CLOCK_HZ: u64 = 1_024_000;

/// Native S-DSP output sample rate in Hz.
const DSP_SAMPLE_RATE_HZ: u64 = 32_000;

/// SPC700 cycles per native S-DSP sample. The DSP emits exactly one
/// 32 kHz sample every 32 SPC cycles (1.024 MHz / 32 kHz = 32); used to
/// pace sample generation against the SPC clock inside `catch_up_to` so
/// DSP register writes are observed at the sample they land on.
const SPC_CYCLES_PER_DSP_SAMPLE: u64 = SPC_CLOCK_HZ / DSP_SAMPLE_RATE_HZ;

/// Capacity of the 32 kHz stereo `i16` ring, in samples (~7-8 NTSC
/// frames). Large enough that a normal catch-up window never overflows
/// before the host drains it; absent a host consumer it simply drops
/// the oldest samples.
const AUDIO_RING_CAPACITY: usize = 8192;

/// Master clock the converters are seeded with at construction. The
/// real region clock (NTSC vs PAL) is bound in [`Apu::reset`], which
/// every boot path runs before the APU is stepped.
const DEFAULT_MASTER_CLOCK_HZ: u64 = 21_477_270;

/// The stems, in index order: each voice through its volumes and the main
/// volume, each voice's output before its volumes (which the master holds
/// through the first eight), and the echo through the echo volume.
const STEM_CHANNELS: [StemChannel; 17] = [
    voice("voice0", 0, true),
    voice("voice1", 1, true),
    voice("voice2", 2, true),
    voice("voice3", 3, true),
    voice("voice4", 4, true),
    voice("voice5", 5, true),
    voice("voice6", 6, true),
    voice("voice7", 7, true),
    voice("voice0-dry", 0, false),
    voice("voice1-dry", 1, false),
    voice("voice2-dry", 2, false),
    voice("voice3-dry", 3, false),
    voice("voice4-dry", 4, false),
    voice("voice5-dry", 5, false),
    voice("voice6-dry", 6, false),
    voice("voice7-dry", 7, false),
    StemChannel {
        name: "echo",
        chip: Some(0),
        channel: None,
        stereo: true,
        in_master: true,
    },
];

/// The first dry stem: voice `n`'s is `DRY_STEMS + n`.
const DRY_STEMS: usize = 8;

/// A voice's stem: through its volumes (stereo, one of the master's
/// parts), or before them (mono, dry).
const fn voice(name: &'static str, channel: u8, stereo: bool) -> StemChannel {
    StemChannel {
        name,
        chip: Some(0),
        channel: Some(channel),
        stereo,
        in_master: stereo,
    }
}

/// The stems the host armed on the S-DSP and what they recorded: each
/// sample's voices and echo beside the sample the DSP output, and each
/// voice's last dry samples for its waveform. A host setting, not saved.
pub(crate) struct ApuStems {
    recorder: StemRecorder<i16>,
    /// Voice 0's left amount and dry output for the next sample.
    carry: [i32; 2],
    /// Whether `carry` follows the DSP: a load or a reset puts the DSP
    /// somewhere the carry did not come from, and the sample after it
    /// departs.
    carried: bool,
    scopes: [ScopeRing; 8],
}

impl ApuStems {
    fn new() -> Self {
        Self {
            recorder: StemRecorder::new(&Apu::stem_layout(), DSP_SAMPLE_RATE_HZ as u32 * 2),
            carry: [0; 2],
            carried: false,
            scopes: std::array::from_fn(|_| ScopeRing::new()),
        }
    }

    /// One sample, `master` the one the DSP output.
    #[inline]
    pub(in crate::apu) fn record(&mut self, dsp: &dsp::SDsp, master: (i16, i16)) {
        let mut values = [0i16; dsp::STEM_VALUES];
        let mut departs = dsp.stem_frame(&mut self.carry, master, &mut values);
        departs |= !std::mem::replace(&mut self.carried, true);
        for (scope, &dry) in self.scopes.iter_mut().zip(&values[16..24]) {
            scope.push(f32::from(dry) / 32768.0);
        }
        self.recorder.push(&values, [master.0, master.1], departs);
    }
}

/// The Audio Processing Unit.
///
/// Owns the SPC700 and the machine state it addresses ([`ApuState`]),
/// kept in separate fields so `catch_up_to` can step the CPU against a
/// mutable borrow of the state (the "borrow split").
pub struct Apu {
    spc700: Spc700,
    state: ApuState,
    /// Maps master cycles -> cumulative SPC700 cycles.
    spc_sync: IntegerRateConverter,
    /// Maps master cycles -> cumulative 32 kHz samples.
    dsp_clock: IntegerRateConverter,
    /// SPC700 cycles actually executed since reset. Tracks the
    /// `spc_sync` target within one instruction (it may overshoot the
    /// target by the length of the last instruction, never more).
    apu_cycles: u64,
    /// Cumulative 32 kHz samples emitted since reset. Exact function of
    /// the master cycle, so it never drifts.
    dsp_samples_emitted: u64,
    /// 32 kHz stereo `i16` output (left, right interleaved) the DSP
    /// fills as samples come due.
    audio_ring: AudioRingBuffer<i16>,
    /// When set, [`Apu::catch_up_to`] captures a [`TraceEntryBinarySpc700`]
    /// before every SPC700 step. The emulator syncs this from the
    /// debugger's per-CPU tracing flag each frame.
    spc_trace_enabled: bool,
    /// SPC700 trace records captured this catch-up window, drained by the
    /// emulator into the debugger's tracing buffer (the debugger is not
    /// reachable from inside `catch_up_to`, which runs deep in a mailbox
    /// access).
    spc_trace_buf: Vec<TraceEntryBinarySpc700>,
    /// The stems, once the host armed any.
    stems: Option<Box<ApuStems>>,
    /// What the key-on latch kept, until the host takes it.
    key_on_state: Option<Box<crate::savestate::ApuSaveState>>,
}

/// The key-on latch, to carry over to the APU a load builds: whether it
/// waits, and what it kept.
pub(crate) struct KeyOnLatch {
    armed: bool,
    kept: Option<Box<crate::savestate::ApuSaveState>>,
}

impl Apu {
    /// Build a powered-on APU: zeroed ARAM, IPL ROM mapped in, and the
    /// SPC700 booted from the IPL reset vector. The converters are
    /// seeded with the NTSC master clock; [`Apu::reset`] rebinds them
    /// to the actual region clock.
    pub fn new() -> Self {
        let mut apu = Self {
            spc700: Spc700::new(),
            state: ApuState::new(),
            spc_sync: IntegerRateConverter::new(SPC_CLOCK_HZ, DEFAULT_MASTER_CLOCK_HZ),
            dsp_clock: IntegerRateConverter::new(DSP_SAMPLE_RATE_HZ, DEFAULT_MASTER_CLOCK_HZ),
            apu_cycles: 0,
            dsp_samples_emitted: 0,
            audio_ring: AudioRingBuffer::with_capacity(AUDIO_RING_CAPACITY),
            spc_trace_enabled: false,
            spc_trace_buf: Vec::new(),
            stems: None,
            key_on_state: None,
        };
        apu.boot_spc();
        apu
    }

    /// Reset the APU and rebind the rate converters to `master_clock_hz`
    /// (so PAL and NTSC consoles produce the right sample cadence).
    /// ARAM is preserved; the register-level state, the SPC700, and
    /// both converters are re-anchored to cycle 0.
    pub fn reset(&mut self, master_clock_hz: u64) {
        self.state.reset_registers();
        self.spc_sync = IntegerRateConverter::new(SPC_CLOCK_HZ, master_clock_hz);
        self.dsp_clock = IntegerRateConverter::new(DSP_SAMPLE_RATE_HZ, master_clock_hz);
        self.apu_cycles = 0;
        self.dsp_samples_emitted = 0;
        self.audio_ring.clear();
        self.boot_spc();
        self.stems_lose_track();
        self.sound_log_interrupted();
    }

    /// Fetch the SPC700 reset vector (from the IPL ROM, mapped in by
    /// the power-on CONTROL value) and start execution there.
    fn boot_spc(&mut self) {
        let mut bus = ApuBus {
            state: &mut self.state,
        };
        self.spc700.reset_with_bus(&mut bus);
    }

    /// Advance the APU up to the master cycle `master_cycles`.
    ///
    /// Steps the SPC700 (ticking the timers with each instruction's
    /// cycle cost) until it reaches the corresponding SPC cycle,
    /// interleaving 32 kHz sample generation as the SPC clock advances
    /// (one sample per [`SPC_CYCLES_PER_DSP_SAMPLE`] SPC cycles) so a DSP
    /// register write is observed at the sample it lands on rather than
    /// coalesced against the window's final state. Both halves are
    /// re-anchored to the *ideal* target so the small per-instruction
    /// overshoot in the SPC clock can never accumulate into drift, and
    /// the window's sample total stays pinned to the drift-free
    /// `dsp_clock` count.
    pub fn catch_up_to(&mut self, master_cycles: u64) {
        // Catch-up is monotonic. A target at or behind the anchor we
        // already advanced to is a no-op, this happens routinely
        // because the mailbox `apu_cycle` lags `master_cycles` whenever
        // an event handler (HDMA, the FrameEnd hook) bumps the master
        // clock without refreshing the per-instruction snapshot. Both
        // converters share the same anchor, so one check covers both.
        if master_cycles <= self.spc_sync.reference_cycles() {
            return;
        }
        let spc_target = self.spc_sync.cycles_to_samples(master_cycles);
        // Cumulative 32 kHz samples due by this master cycle, the
        // drift-free authority for the window's total. Computed up front
        // so the step loop below can interleave sample generation as the
        // SPC clock advances, instead of batching it after every register
        // write has already landed.
        let dsp_target = self.dsp_clock.cycles_to_samples(master_cycles);
        while self.apu_cycles < spc_target {
            if self.spc_trace_enabled {
                self.capture_spc_trace_entry();
            }
            let cycles = {
                let mut bus = ApuBus {
                    state: &mut self.state,
                };
                self.spc700.step(&mut bus)
            }
            // Every opcode costs at least one cycle; guard the loop
            // against a pathological zero so it always terminates.
            .max(1);
            self.apu_cycles += cycles as u64;
            self.state.timer0.tick(cycles);
            self.state.timer1.tick(cycles);
            self.state.timer2.tick(cycles);
            if self.state.key_on_written {
                self.keep_key_on_state();
            }

            // Emit the samples due as of the SPC cycle just reached, so the
            // DSP register writes the instruction just made (a voice
            // key-on above all) are observed at the sample they land on
            // rather than coalesced against the window's final state. The
            // DSP advances one sample per `SPC_CYCLES_PER_DSP_SAMPLE`; clamp
            // to the window's drift-free target so the total never changes.
            let due = (self.apu_cycles / SPC_CYCLES_PER_DSP_SAMPLE).min(dsp_target);
            if due > self.dsp_samples_emitted {
                let stems = self.stems.as_deref_mut().filter(|s| s.recorder.is_armed());
                self.state
                    .run_dsp(due - self.dsp_samples_emitted, &mut self.audio_ring, stems);
                self.dsp_samples_emitted = due;
            }
        }
        self.spc_sync.advance(master_cycles, spc_target);

        // Flush any remaining samples up to the exact drift-free target.
        // The SPC clock can stop a sample or two short of the sample clock
        // at the window edge (the two converters round independently), so
        // this top-up keeps the cumulative count exact and the cadence
        // drift-free over any run length.
        if dsp_target > self.dsp_samples_emitted {
            let stems = self.stems.as_deref_mut().filter(|s| s.recorder.is_armed());
            self.state.run_dsp(
                dsp_target - self.dsp_samples_emitted,
                &mut self.audio_ring,
                stems,
            );
            self.dsp_samples_emitted = dsp_target;
        }
        self.dsp_clock.advance(master_cycles, dsp_target);
    }

    /// Capture the SPC700's pre-execution state and instruction bytes
    /// into [`Apu::spc_trace_buf`]. Reads the opcode and its operands via
    /// the side-effect-free [`ApuState::read_code_byte`] so tracing never
    /// perturbs timers or DSP ports. The instruction length comes from
    /// the shared SPC700 opcode table, the same source the disassembler
    /// uses, so the captured byte count always matches what the trace
    /// formatter will decode.
    fn capture_spc_trace_entry(&mut self) {
        let pc = self.spc700.pc;
        let opcode = self.state.read_code_byte(pc);
        let len = spc700::decode::OPCODE_TABLE[opcode as usize]
            .mode
            .operand_size()
            .min(3);
        let mut bytes = [0u8; 3];
        for (i, slot) in bytes.iter_mut().enumerate().take(len as usize) {
            *slot = self.state.read_code_byte(pc.wrapping_add(i as u16));
        }
        self.spc_trace_buf.push(TraceEntryBinarySpc700 {
            master_cycle: self.apu_cycles,
            pc,
            a: self.spc700.a,
            x: self.spc700.x,
            y: self.spc700.y,
            sp: self.spc700.sp,
            psw: self.spc700.psw,
            bytes_len: len,
            bytes,
        });
    }

    /// Enable or disable SPC700 instruction tracing. When disabled, any
    /// records already captured are discarded so a stale tail can't leak
    /// into a later session.
    pub fn set_spc_trace_enabled(&mut self, enabled: bool) {
        self.spc_trace_enabled = enabled;
        if !enabled {
            self.spc_trace_buf.clear();
        }
    }

    /// Drain the SPC700 trace records captured since the last call,
    /// leaving the buffer empty for the next catch-up window.
    pub fn take_spc_trace(&mut self) -> Vec<TraceEntryBinarySpc700> {
        std::mem::take(&mut self.spc_trace_buf)
    }

    /// Arm or disarm capture of S-DSP register-write events (a `$F3`
    /// DSPDATA write records the selected DSP register index). The
    /// emulator mirrors the debugger's "has watch/event breakpoints"
    /// state into this each frame, the same bridge shape as the SPC700
    /// trace flag. Disarming discards any pending records.
    pub fn set_dsp_event_recording(&mut self, enabled: bool) {
        self.state.dsp_event_armed = enabled;
        if !enabled {
            self.state.dsp_write_events.clear();
        }
    }

    /// Drain the S-DSP register indices written since the last call
    /// (newest last), leaving the buffer empty for the next window.
    pub fn take_dsp_events(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.state.dsp_write_events)
    }

    /// Side-effect-free SPC700 code read for the disassembler / debugger.
    /// Resolves only the IPL-ROM window + ARAM, never a DSP data port or
    /// a read-clearing timer output, so a windowed disassembly can't
    /// perturb the running program.
    pub fn spc_peek(&self, addr: u16) -> u8 {
        self.state.read_code_byte(addr)
    }

    /// Main-CPU read of a mailbox port (`$2140-$2143`). Catches the
    /// SPC700 up first so the read observes its latest write.
    pub fn read_port(&mut self, port: u8, master_cycles: u64) -> u8 {
        self.catch_up_to(master_cycles);
        self.state.ports_apu_to_cpu[(port & 0x03) as usize]
    }

    /// Main-CPU write of a mailbox port (`$2140-$2143`). Catches the
    /// SPC700 up first so the write is ordered correctly against the
    /// SPC's own port traffic; while the sound log takes the mailbox down,
    /// the write goes into it at the sample the catch-up reached.
    pub fn write_port(&mut self, port: u8, value: u8, master_cycles: u64) {
        self.catch_up_to(master_cycles);
        let port = port & 0x03;
        self.state.ports_cpu_to_apu[usize::from(port)] = value;
        if let Some(log) = self.state.sound_log.as_deref_mut() {
            if log.logger.logs_mailbox() {
                log.logger.extend([SoundLogEntry {
                    time: log.samples,
                    chip: 0,
                    event: SoundEvent::Mailbox {
                        port: u16::from(port),
                        value,
                    },
                }]);
            }
        }
    }

    /// Read-only snapshot of the four SPC700 -> main-CPU mailbox ports
    /// (`$2140-$2143`). Unlike [`Self::read_port`] this neither catches the
    /// SPC700 up nor takes `&mut self`, so the debugger can poll the
    /// mailbox without perturbing timing. Used by the WASM
    /// `getApuPortValues` export.
    pub fn output_ports(&self) -> [u8; 4] {
        self.state.ports_apu_to_cpu
    }

    /// Cumulative 32 kHz samples emitted since reset.
    pub fn samples_emitted(&self) -> u64 {
        self.dsp_samples_emitted
    }

    /// Number of 32 kHz `i16` samples currently queued in the audio
    /// ring (left/right interleaved, so a stereo frame counts as two).
    pub fn audio_samples_available(&self) -> usize {
        self.audio_ring.available()
    }

    /// Drop every queued 32 kHz sample without delivering it.
    ///
    /// The ring is where a headless frame's output waits: `catch_up_to`
    /// always runs the DSP, and only the drain to the host ring is gated on
    /// presentation. After a rollback re-simulation that queue holds audio
    /// for frames the player has **already heard** in their mispredicted
    /// form, and the next presented frame would drain all of it at once,
    /// which plays as an echo. Dropping it here is what keeps the
    /// correction silent.
    ///
    /// Not part of the serialized state (`ApuSaveState` deliberately omits
    /// the ring, rebuilding it from the restored DSP), so this cannot move
    /// the netplay desync hash.
    pub fn discard_pending_audio(&mut self) {
        self.audio_ring.clear();
    }

    /// Drain up to `dst.len()` queued 32 kHz `i16` samples into `dst`,
    /// returning how many were copied. Left/right interleaved.
    pub fn drain_audio(&mut self, dst: &mut [i16]) -> usize {
        self.audio_ring.drain_into(dst)
    }

    /// SPC700 cycles executed since reset.
    pub fn spc_cycles(&self) -> u64 {
        self.apu_cycles
    }

    /// Read-only borrow of the SPC700 (debugger / tests).
    pub fn spc700(&self) -> &Spc700 {
        &self.spc700
    }

    /// Read a byte through the SPC700 memory map without stepping the
    /// CPU. For the debugger and tests; note that reading a timer
    /// output (`$FD-$FF`) clears it, exactly as a real SPC read does.
    pub fn spc_read(&mut self, addr: u16) -> u8 {
        let mut bus = ApuBus {
            state: &mut self.state,
        };
        bus.read(addr as u32)
    }

    /// Write a byte through the SPC700 memory map without stepping the
    /// CPU (debugger / tests).
    pub fn spc_write(&mut self, addr: u16, value: u8) {
        let mut bus = ApuBus {
            state: &mut self.state,
        };
        bus.write(addr as u32, value);
    }

    /// Read-only view of ARAM (debugger / tests).
    pub fn aram(&self) -> &[u8] {
        self.state.aram.as_ref()
    }

    /// Read-only view of the S-DSP register file (debugger / tests).
    pub fn dsp_regs(&self) -> &[u8; dsp::DSP_REG_COUNT] {
        self.state.dsp.regs()
    }

    /// Current 4-bit output of timer `index` (0-2) *without* the
    /// read-clear side effect, for inspection by the debugger and
    /// tests (use the SPC700's own `$FD-$FF` reads for hardware
    /// semantics).
    pub fn timer_output(&self, index: usize) -> u8 {
        match index {
            0 => self.state.timer0.peek_output(),
            1 => self.state.timer1.peek_output(),
            _ => self.state.timer2.peek_output(),
        }
    }

    /// Whether timer `index` (0-2) is currently enabled.
    pub fn timer_enabled(&self, index: usize) -> bool {
        match index {
            0 => self.state.timer0.enabled(),
            1 => self.state.timer1.enabled(),
            _ => self.state.timer2.enabled(),
        }
    }

    /// Capture the whole APU, SPC700, ARAM, mailbox ports, aux,
    /// timers, the S-DSP, CONTROL, and both drift-free rate converter
    /// anchors, into a snapshot. The host audio ring is *not* captured:
    /// it refills from the restored DSP on the next catch-up.
    pub fn to_savestate(&self) -> crate::savestate::ApuSaveState {
        crate::savestate::ApuSaveState {
            spc700: crate::savestate::Spc700State::capture(&self.spc700),
            aram: self.state.aram.to_vec(),
            ports_cpu_to_apu: self.state.ports_cpu_to_apu,
            ports_apu_to_cpu: self.state.ports_apu_to_cpu,
            aux: self.state.aux,
            timer0: self.state.timer0.to_savestate(),
            timer1: self.state.timer1.to_savestate(),
            timer2: self.state.timer2.to_savestate(),
            dsp: self.state.dsp.to_savestate(),
            control: self.state.control,
            spc_sync: self.spc_sync.state(),
            dsp_clock: self.dsp_clock.state(),
            apu_cycles: self.apu_cycles,
            dsp_samples_emitted: self.dsp_samples_emitted,
        }
    }

    /// Restore the whole APU from a snapshot. `master_clock_hz` is the
    /// console region clock the rate converters re-derive their ratio
    /// from, the persisted state only carries the drifting anchor, not
    /// the ratio, so a region-correct converter is rebuilt here. The
    /// host audio ring is cleared (it is rebuilt from the restored DSP).
    pub fn from_savestate(&mut self, s: &crate::savestate::ApuSaveState, master_clock_hz: u64) {
        s.spc700.restore(&mut self.spc700);
        self.state.aram.copy_from_slice(&s.aram);
        self.state.ports_cpu_to_apu = s.ports_cpu_to_apu;
        self.state.ports_apu_to_cpu = s.ports_apu_to_cpu;
        self.state.aux = s.aux;
        self.state.timer0.from_savestate(&s.timer0);
        self.state.timer1.from_savestate(&s.timer1);
        self.state.timer2.from_savestate(&s.timer2);
        self.state.dsp.from_savestate(&s.dsp);
        self.state.control = s.control;
        self.spc_sync =
            IntegerRateConverter::from_state(SPC_CLOCK_HZ, master_clock_hz, &s.spc_sync);
        self.dsp_clock =
            IntegerRateConverter::from_state(DSP_SAMPLE_RATE_HZ, master_clock_hz, &s.dsp_clock);
        self.apu_cycles = s.apu_cycles;
        self.dsp_samples_emitted = s.dsp_samples_emitted;
        self.audio_ring.clear();
        self.stems_lose_track();
        self.sound_log_interrupted();
    }

    /// The S-DSP's stems: at its 32 kHz rate, 16-bit samples, each voice's
    /// rounded on its own where the output rounds their sum once (one step
    /// a voice).
    pub fn stem_layout() -> StemLayout {
        StemLayout {
            rate: DSP_SAMPLE_RATE_HZ as u32,
            kind: SampleKind::I16,
            channels: STEM_CHANNELS.to_vec(),
            master: StemChannel {
                name: "master",
                chip: None,
                channel: None,
                stereo: true,
                in_master: false,
            },
            sum: StemSum::WithinLsb(8),
        }
    }

    /// Record `stems` from the next frame's end, or from now when
    /// `between_frames` ([`retro_core::audio::SoundCore::arm_stems`]).
    pub fn arm_stems(&mut self, stems: StemSet, between_frames: bool) {
        let state = self.stems.get_or_insert_with(|| Box::new(ApuStems::new()));
        let was = state.recorder.is_armed();
        state.recorder.arm(stems, between_frames);
        if state.recorder.is_armed() && !was {
            state.carry = self.state.dsp.stem_carry();
            state.carried = true;
        }
    }

    /// The end of a frame for the stems: a frame the host was not shown is
    /// taken back out, and what the host armed since is armed.
    pub fn stems_frame_end(&mut self, presented: bool) {
        if let Some(state) = self.stems.as_deref_mut() {
            let was = state.recorder.is_armed();
            state.recorder.frame_end(presented);
            if state.recorder.is_armed() && !was {
                state.carry = self.state.dsp.stem_carry();
                state.carried = true;
            }
        }
    }

    /// What the stems recorded since the last take, by whole frames.
    pub fn take_stems(&mut self) -> Option<StemTake> {
        self.stems.as_deref_mut()?.recorder.take()
    }

    /// The stems, to carry over to the APU a load builds.
    pub(crate) fn take_stem_state(&mut self) -> Option<Box<ApuStems>> {
        self.stems.take()
    }

    /// The stems carried over from the APU this one replaces.
    pub(crate) fn restore_stem_state(&mut self, stems: Option<Box<ApuStems>>) {
        self.stems = stems;
        self.stems_lose_track();
    }

    /// Voice `voice`'s last dry samples while the host takes its dry stem.
    pub(crate) fn voice_scope(&self, voice: usize) -> Option<&ScopeRing> {
        let state = self.stems.as_deref()?;
        state
            .recorder
            .armed()
            .contains(DRY_STEMS + voice)
            .then(|| &state.scopes[voice])
    }

    /// Start a sound log holding `contents`, its first entries at time 0
    /// the DSP's 128 registers (with the writes) and a key-on for each
    /// voice sounding (with the keys)
    /// ([`retro_core::audio::SoundCore::start_sound_log`]).
    pub fn start_sound_log(&mut self, contents: SoundLogContents) {
        if !contents.any() {
            self.stop_sound_log();
            return;
        }
        let mut log = Box::<DspLog>::default();
        let image = log.image(&self.state.dsp, contents);
        log.logger.start(contents, image);
        self.state.sound_log = Some(log);
        self.state.dsp.set_keys_logged(contents.keys);
    }

    /// Stop the log; what it logged waits for the next take.
    pub fn stop_sound_log(&mut self) {
        if let Some(log) = self.state.sound_log.as_deref_mut() {
            log.logger.stop();
        }
        self.state.dsp.set_keys_logged(false);
    }

    /// What the log took down since the last take, on the DSP's 32 kHz
    /// sample clock.
    pub fn take_sound_log(&mut self) -> Option<SoundLog> {
        let log = self.state.sound_log.as_deref_mut()?;
        let take = log.logger.take(DSP_SAMPLE_RATE_HZ);
        if !log.logger.is_on() {
            self.state.sound_log = None;
        }
        take
    }

    /// A frame's end for the log.
    pub fn sound_log_frame_end(&mut self) {
        if let Some(log) = self.state.sound_log.as_deref_mut() {
            if log.logger.is_on() {
                let time = log.samples;
                log.logger.extend([SoundLogEntry {
                    time,
                    chip: 0,
                    event: SoundEvent::FrameEnd,
                }]);
            }
        }
    }

    /// The log, to carry over to the APU a load builds.
    pub(crate) fn take_sound_log_state(&mut self) -> Option<Box<DspLog>> {
        self.state.sound_log.take()
    }

    /// The log carried over from the APU this one replaces; the reset a
    /// load ends with marks it interrupted.
    pub(crate) fn restore_sound_log_state(&mut self, log: Option<Box<DspLog>>) {
        let keys = log.as_deref().is_some_and(|log| log.logger.logs_keys());
        self.state.sound_log = log;
        self.state.dsp.set_keys_logged(keys);
    }

    /// A reset, a load or a savestate's load came: the next take says so,
    /// and the log goes on with the DSP as it stands, what the log holds of
    /// it.
    fn sound_log_interrupted(&mut self) {
        if let Some(log) = self.state.sound_log.as_deref_mut() {
            if log.logger.is_on() {
                log.logger.interrupt();
                let image = log.image(&self.state.dsp, log.logger.contents());
                log.logger.extend(image);
            }
        }
    }

    /// The page of ARAM the DSP's directory register names.
    pub fn sample_directory_page(&self) -> u8 {
        self.state.dsp.directory_register()
    }

    /// The sample directory the DSP's registers name: each source number's
    /// start and loop addresses in ARAM, and the voices whose SRCN names it.
    pub fn sample_directory(&self) -> Vec<DirectoryEntry> {
        directory(
            &self.state.aram[..],
            self.state.dsp.directory_register(),
            self.state.dsp.sources(),
        )
    }

    /// From now on, keep the APU's state as it stands after the first
    /// SPC700 instruction that writes KON with a voice's bit set, the
    /// instruction's cycles counted on the timers, before the samples they
    /// are due; the latch then waits no more. What it kept before is
    /// dropped.
    pub fn arm_key_on_latch(&mut self) {
        self.state.key_on_armed = true;
        self.state.key_on_written = false;
        self.key_on_state = None;
    }

    /// Whether the key-on latch waits for a key-on.
    pub fn key_on_latch_armed(&self) -> bool {
        self.state.key_on_armed
    }

    /// The state the key-on latch kept, once; `None` while it waits or was
    /// never armed.
    pub fn take_key_on_state(&mut self) -> Option<crate::savestate::ApuSaveState> {
        self.key_on_state.take().map(|state| *state)
    }

    /// The key-on came: keep the state, and wait no more.
    fn keep_key_on_state(&mut self) {
        self.state.key_on_written = false;
        self.state.key_on_armed = false;
        self.key_on_state = Some(Box::new(self.to_savestate()));
    }

    /// The key-on latch, to carry over to the APU a load builds.
    pub(crate) fn take_key_on_latch(&mut self) -> KeyOnLatch {
        KeyOnLatch {
            armed: std::mem::take(&mut self.state.key_on_armed),
            kept: self.key_on_state.take(),
        }
    }

    /// The key-on latch carried over from the APU this one replaces.
    pub(crate) fn restore_key_on_latch(&mut self, latch: KeyOnLatch) {
        self.state.key_on_armed = latch.armed;
        self.key_on_state = latch.kept;
    }

    /// The DSP is somewhere the stems' carry did not follow it.
    fn stems_lose_track(&mut self) {
        if let Some(state) = self.stems.as_deref_mut() {
            state.carried = false;
        }
    }
}

impl Default for Apu {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boots_spc_into_the_ipl() {
        let apu = Apu::new();
        // The IPL reset vector is $FFC0; the SPC700 must start there.
        assert_eq!(apu.spc700().pc, 0xFFC0);
    }

    #[test]
    fn catch_up_to_zero_does_not_run_the_spc() {
        let mut apu = Apu::new();
        apu.catch_up_to(0);
        assert_eq!(apu.spc_cycles(), 0);
        assert_eq!(apu.samples_emitted(), 0);
    }

    #[test]
    fn ipl_writes_ready_bytes_to_output_ports() {
        let mut apu = Apu::new();
        // The IPL first clears ~$EF bytes of zero page (a ~2400-cycle
        // loop) before writing its $AA/$BB handshake, so give it a
        // comfortable budget of SPC cycles.
        let master = 8000 * DEFAULT_MASTER_CLOCK_HZ / SPC_CLOCK_HZ;
        apu.catch_up_to(master);
        assert_eq!(apu.read_port(0, master), 0xAA);
        assert_eq!(apu.read_port(1, master), 0xBB);
    }

    #[test]
    fn cpu_to_apu_port_is_visible_to_the_spc() {
        let mut apu = Apu::new();
        apu.write_port(2, 0x5C, 0);
        assert_eq!(apu.spc_read(0x00F6), 0x5C);
    }

    /// While the log takes the mailbox down, each write of the main CPU's
    /// is logged at its port with the samples the S-DSP made by then; a
    /// read, and the SPC700's own writes back, are not the mailbox's.
    #[test]
    fn the_mailbox_s_writes_are_logged_at_the_sample_they_came() {
        let mut apu = Apu::new();
        apu.start_sound_log(SoundLogContents {
            mailbox: true,
            ..SoundLogContents::default()
        });
        let mut samples = Vec::new();
        apu.write_port(0, 0x11, 7_000);
        samples.push(apu.samples_emitted());
        apu.read_port(1, 14_000);
        apu.write_port(7, 0x22, 21_000);
        samples.push(apu.samples_emitted());
        apu.spc_write(0x00F5, 0x33);
        apu.write_port(1, 0x44, 21_000);
        samples.push(apu.samples_emitted());
        assert!(0 < samples[0] && samples[0] < samples[1]);
        let log = apu.take_sound_log().expect("a log runs");
        let mailbox = |time, port, value| SoundLogEntry {
            time,
            chip: 0,
            event: SoundEvent::Mailbox { port, value },
        };
        assert_eq!(
            log.entries,
            [
                mailbox(samples[0], 0, 0x11),
                mailbox(samples[1], 3, 0x22),
                mailbox(samples[2], 1, 0x44)
            ]
        );
        assert_eq!(apu.spc_read(0x00F7), 0x22, "the port took it");
        apu.start_sound_log(SoundLogContents {
            writes: true,
            ..SoundLogContents::default()
        });
        apu.write_port(2, 0x55, 28_000);
        let log = apu.take_sound_log().expect("a log runs");
        assert!(
            !log.entries
                .iter()
                .any(|entry| matches!(entry.event, SoundEvent::Mailbox { .. })),
            "a log without the mailbox"
        );
    }

    #[test]
    fn apu_to_cpu_port_is_visible_to_the_cpu() {
        let mut apu = Apu::new();
        apu.spc_write(0x00F5, 0x9D);
        assert_eq!(apu.read_port(1, 0), 0x9D);
    }

    #[test]
    fn sample_cadence_is_exact_and_drift_free() {
        let mut apu = Apu::new();
        // A fresh converter computing the same thing in one shot is the
        // ground truth; advancing in many irregular steps must agree
        // with it exactly.
        let reference = IntegerRateConverter::new(DSP_SAMPLE_RATE_HZ, DEFAULT_MASTER_CLOCK_HZ);
        let mut master = 0u64;
        for step in 1..=5000u64 {
            master += 300 + (step % 37); // irregular master-cycle deltas
            apu.catch_up_to(master);
            assert_eq!(apu.samples_emitted(), reference.cycles_to_samples(master));
        }
    }

    #[test]
    fn reset_rebinds_clock_and_reanchors() {
        let mut apu = Apu::new();
        apu.catch_up_to(1_000_000);
        assert!(apu.samples_emitted() > 0);
        apu.reset(DEFAULT_MASTER_CLOCK_HZ);
        assert_eq!(apu.samples_emitted(), 0);
        assert_eq!(apu.spc_cycles(), 0);
        assert_eq!(apu.spc700().pc, 0xFFC0);
    }

    /// A music driver keys a voice on and then clears KON a short while
    /// later, both inside a single catch-up window. The DSP must observe
    /// the key-on at the sample it lands on. If sample generation were
    /// batched after the whole SPC window ran, KON would already be back
    /// to 0 by the time any sample is produced and the voice would never
    /// key on: every note in a busy frame would be silently dropped. We
    /// assert voice 0's ENVX climbs above 0, proving it keyed on and ran
    /// its envelope within the window.
    /// The key-on latch keeps the APU as the instruction that writes KON
    /// leaves it: the program counter past that instruction, KON written;
    /// it keeps nothing before, and nothing more after.
    #[test]
    fn the_key_on_latch_keeps_the_state_after_the_instruction() {
        #[rustfmt::skip]
        let program: [u8; 14] = [
            0x8F, 0x4C, 0xF2, // mov $f2,#$4c   DSPADDR = KON
            0x8F, 0x00, 0xF3, // mov $f3,#$00   KON = 0: no voice, no key-on
            0x8F, 0x05, 0xF3, // mov $f3,#$05   KON = voices 0 and 2
            0x8F, 0x01, 0xF3, // mov $f3,#$01   KON = voice 0
            0x2F, 0xFE,       // bra -2
        ];
        let mut apu = Apu::new();
        apu.state.aram[0x0200..0x0200 + program.len()].copy_from_slice(&program);
        apu.spc700.pc = 0x0200;
        assert!(apu.take_key_on_state().is_none(), "never armed");
        apu.arm_key_on_latch();
        assert!(apu.key_on_latch_armed());
        apu.catch_up_to(400 * DEFAULT_MASTER_CLOCK_HZ / SPC_CLOCK_HZ);
        assert!(!apu.key_on_latch_armed(), "the latch waits no more");
        let kept = apu.take_key_on_state().expect("the key-on was kept");
        assert_eq!(kept.spc700.pc, 0x0209, "after the instruction");
        assert_eq!(kept.dsp.regs[0x4C], 0x05);
        assert!(apu.take_key_on_state().is_none(), "taken once");
    }

    #[test]
    fn key_on_pulse_within_one_window_is_not_coalesced() {
        // Tiny SPC700 routine at $0200. All DSP access goes through the
        // $F2/$F3 (DSPADDR/DSPDATA) ports; `mov $dp,#imm` encodes the
        // immediate first, then the direct-page address (`8F imm dp`).
        #[rustfmt::skip]
        let program: [u8; 31] = [
            0x20,             // clrp                  direct page = $00xx
            0x8F, 0x6C, 0xF2, // mov $f2,#$6c          DSPADDR = FLG
            0x8F, 0x00, 0xF3, // mov $f3,#$00          FLG = 0 (leave soft-reset/mute)
            0x8F, 0x07, 0xF2, // mov $f2,#$07          DSPADDR = voice0 GAIN
            0x8F, 0x7F, 0xF3, // mov $f3,#$7f          GAIN = 0x7f (direct level)
            0x8F, 0x4C, 0xF2, // mov $f2,#$4c          DSPADDR = KON
            0x8F, 0x01, 0xF3, // mov $f3,#$01          KON = voice 0
            0x8D, 0x80,       // mov y,#$80
            0xFE, 0xFE,       // dbnz y,-2             ~512-cycle delay loop
            0x8F, 0x4C, 0xF2, // mov $f2,#$4c          DSPADDR = KON
            0x8F, 0x00, 0xF3, // mov $f3,#$00          KON = 0 (clear the edge)
            0x2F, 0xFE,       // bra -2                idle forever
        ];

        let mut apu = Apu::new();
        const PROGRAM_BASE: usize = 0x0200;
        for (i, byte) in program.iter().enumerate() {
            apu.state.aram[PROGRAM_BASE + i] = *byte;
        }
        apu.spc700.pc = PROGRAM_BASE as u16;

        // One window covering ~3000 SPC cycles, well past the routine, so
        // the key-on, the delay, and the KON clear all fall inside it.
        let master = 3000 * DEFAULT_MASTER_CLOCK_HZ / SPC_CLOCK_HZ;
        apu.catch_up_to(master);

        // Voice 0 ENVX is register $08; it is non-zero only if the voice
        // keyed on and its GAIN envelope committed during the window.
        const VOICE0_ENVX: usize = 0x08;
        assert!(
            apu.dsp_regs()[VOICE0_ENVX] > 0,
            "voice 0 must key on within one window (ENVX = {})",
            apu.dsp_regs()[VOICE0_ENVX],
        );
    }
}
