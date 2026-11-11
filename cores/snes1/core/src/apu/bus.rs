//! The SPC700 memory map and the machine state behind it.
//!
//! [`ApuState`] holds everything the SPC700 talks to *except* the CPU
//! itself: the 64 KiB of ARAM, the four CPU mailbox ports (both
//! directions), the two auxiliary registers, the three timers, the
//! DSP register file, and the CONTROL register. [`ApuBus`] is a thin
//! `&mut ApuState` wrapper that implements [`Addressable`] so the
//! SPC700 can be stepped against it.
//!
//! Splitting the state out of [`crate::apu::Apu`] is what makes the
//! borrow check work: `Apu::catch_up_to` steps `self.spc700` while
//! `ApuBus` holds `&mut self.state`, two disjoint fields of the same
//! struct, exactly like the 5A22 emulator steps its CPU against its
//! bus.
//!
//! ## Memory map
//!
//! | Range          | Contents                                     |
//! |----------------|----------------------------------------------|
//! | `$0000-$00EF`  | Zero/low page ARAM                           |
//! | `$00F0`        | TEST (write-only; reads open bus)            |
//! | `$00F1`        | CONTROL (write-only; timer enables + IPL)    |
//! | `$00F2`        | DSPADDR                                      |
//! | `$00F3`        | DSPDATA                                      |
//! | `$00F4-$00F7`  | CPU mailbox (read = CPU->APU, write = APU->CPU) |
//! | `$00F8-$00F9`  | AUX registers (plain R/W)                    |
//! | `$00FA-$00FC`  | Timer 0/1/2 targets (write-only)             |
//! | `$00FD-$00FF`  | Timer 0/1/2 outputs (read-clears)            |
//! | `$0100-$FFBF`  | ARAM                                         |
//! | `$FFC0-$FFFF`  | IPL ROM if CONTROL bit 7 set, else ARAM      |

use retro_core::audio::AudioRingBuffer;
use retro_core::cpu::Addressable;

use super::dsp::SDsp;
use super::ipl::{IPL_BASE, IPL_ROM};
use super::timers::{ApuTimer, DIVISOR_64KHZ, DIVISOR_8KHZ};

/// Size of the APU's audio RAM (64 KiB).
pub const ARAM_SIZE: usize = 0x10000;

/// CONTROL (`$F1`) bit that maps the IPL ROM over `$FFC0-$FFFF`. Set
/// at power-on so the chip boots into the IPL.
const CONTROL_IPL_ENABLE: u8 = 0x80;

/// Everything the SPC700 addresses, minus the SPC700 itself.
pub(super) struct ApuState {
    /// 64 KiB of audio RAM.
    pub(super) aram: Box<[u8; ARAM_SIZE]>,
    /// Mailbox latches the main CPU writes and the SPC700 reads
    /// (`$2140-$2143` on the 5A22 side, `$F4-$F7` reads on the APU
    /// side).
    pub(super) ports_cpu_to_apu: [u8; 4],
    /// Mailbox latches the SPC700 writes and the main CPU reads
    /// (`$F4-$F7` writes on the APU side, `$2140-$2143` reads on the
    /// 5A22 side).
    pub(super) ports_apu_to_cpu: [u8; 4],
    /// `$F8`/`$F9` auxiliary registers, plain read/write byte cells.
    pub(super) aux: [u8; 2],
    pub(super) timer0: ApuTimer,
    pub(super) timer1: ApuTimer,
    pub(super) timer2: ApuTimer,
    pub(super) dsp: SDsp,
    /// CONTROL (`$F1`). Only bit 7 (IPL-ROM enable) is read back here;
    /// the timer-enable bits live in the timers and the port-clear
    /// bits are edge-strobes handled at write time.
    pub(super) control: u8,
    /// When set, a `$F3` (DSPDATA) write appends the selected DSP
    /// register index to [`Self::dsp_write_events`] so the emulator can
    /// match a `DspRegisterWrite` event breakpoint. Armed by the
    /// debugger only when a watch/event breakpoint exists.
    pub(super) dsp_event_armed: bool,
    /// DSP register indices written since the last drain (newest last).
    /// Always empty unless [`Self::dsp_event_armed`] is set.
    pub(super) dsp_write_events: Vec<u8>,
}

impl ApuState {
    /// Power-on state: zeroed ARAM, cleared ports, IPL ROM mapped in.
    pub(super) fn new() -> Self {
        Self {
            aram: Box::new([0u8; ARAM_SIZE]),
            ports_cpu_to_apu: [0; 4],
            ports_apu_to_cpu: [0; 4],
            aux: [0; 2],
            timer0: ApuTimer::new(DIVISOR_8KHZ),
            timer1: ApuTimer::new(DIVISOR_8KHZ),
            timer2: ApuTimer::new(DIVISOR_64KHZ),
            dsp: SDsp::new(),
            control: CONTROL_IPL_ENABLE,
            dsp_event_armed: false,
            dsp_write_events: Vec::new(),
        }
    }

    /// Reset the register-level state (ports, timers, DSP, CONTROL)
    /// while preserving ARAM, matching how a console reset leaves the
    /// audio RAM intact.
    pub(super) fn reset_registers(&mut self) {
        self.ports_cpu_to_apu = [0; 4];
        self.ports_apu_to_cpu = [0; 4];
        self.aux = [0; 2];
        self.timer0 = ApuTimer::new(DIVISOR_8KHZ);
        self.timer1 = ApuTimer::new(DIVISOR_8KHZ);
        self.timer2 = ApuTimer::new(DIVISOR_64KHZ);
        self.dsp.reset();
        self.control = CONTROL_IPL_ENABLE;
        // Drop any captured DSP-write events; the debugger re-arms each
        // frame. `dsp_event_armed` is left as-is, the next sync sets it.
        self.dsp_write_events.clear();
    }

    /// Whether the IPL ROM is currently mapped over `$FFC0-$FFFF`.
    pub(super) fn ipl_enabled(&self) -> bool {
        self.control & CONTROL_IPL_ENABLE != 0
    }

    /// Read an instruction byte for the debugger/trace **without any
    /// MMIO side effects**. Unlike [`ApuBus::read`], this resolves only
    /// the IPL ROM window and the underlying ARAM, it never touches a
    /// DSP data port or a read-clearing timer output, so capturing a
    /// trace can never perturb the running program. Code never executes
    /// from the `$F0-$FF` register page, so reading ARAM there (rather
    /// than the registers) is both safe and the natural disassembly
    /// view.
    pub(super) fn read_code_byte(&self, addr: u16) -> u8 {
        if addr >= IPL_BASE && self.ipl_enabled() {
            IPL_ROM[(addr - IPL_BASE) as usize]
        } else {
            self.aram[addr as usize]
        }
    }

    /// Generate `samples` stereo 32 kHz samples and push them to `ring`.
    ///
    /// The DSP reads its BRR data and sample directory straight from
    /// ARAM, so `dsp` and `aram` are borrowed disjointly here (two
    /// fields of the same state). `aram` is taken mutably because the
    /// echo unit writes its ring back into it.
    pub(super) fn run_dsp(&mut self, samples: u64, ring: &mut AudioRingBuffer<i16>) {
        for _ in 0..samples {
            let (l, r) = self.dsp.run_sample(&mut self.aram[..]);
            ring.push_slice(&[l, r]);
        }
    }

    /// Apply a CONTROL (`$F1`) write: timer enable edges, the two
    /// port-clear strobes, and the persistent IPL-enable bit.
    fn write_control(&mut self, value: u8) {
        self.timer0.set_enabled(value & 0x01 != 0);
        self.timer1.set_enabled(value & 0x02 != 0);
        self.timer2.set_enabled(value & 0x04 != 0);
        // Bit 4 clears the CPU->APU latches for ports 0/1, bit 5 for
        // ports 2/3. These are write-strobes: they clear the latch
        // the SPC700 reads, they are not stored.
        if value & 0x10 != 0 {
            self.ports_cpu_to_apu[0] = 0;
            self.ports_cpu_to_apu[1] = 0;
        }
        if value & 0x20 != 0 {
            self.ports_cpu_to_apu[2] = 0;
            self.ports_cpu_to_apu[3] = 0;
        }
        self.control = value;
    }
}

/// SPC700-facing view of [`ApuState`]. Lives only for the duration of
/// a single `spc700.step`, borrowing the state mutably.
pub(super) struct ApuBus<'a> {
    pub(super) state: &'a mut ApuState,
}

impl Addressable for ApuBus<'_> {
    fn read(&mut self, addr: u32) -> u8 {
        let a = (addr & 0xFFFF) as u16;
        match a {
            0x00F0 => 0,                          // TEST: write-only
            0x00F1 => 0,                          // CONTROL: write-only
            0x00F2 => self.state.dsp.addr(),      // DSPADDR
            0x00F3 => self.state.dsp.read_data(), // DSPDATA
            0x00F4..=0x00F7 => self.state.ports_cpu_to_apu[(a - 0x00F4) as usize],
            0x00F8 => self.state.aux[0],
            0x00F9 => self.state.aux[1],
            0x00FA..=0x00FC => 0, // timer targets: write-only
            0x00FD => self.state.timer0.read_output(),
            0x00FE => self.state.timer1.read_output(),
            0x00FF => self.state.timer2.read_output(),
            IPL_BASE..=0xFFFF if self.state.ipl_enabled() => IPL_ROM[(a - IPL_BASE) as usize],
            // Everything else (including the IPL window when disabled)
            // is plain ARAM.
            _ => self.state.aram[a as usize],
        }
    }

    fn write(&mut self, addr: u32, value: u8) {
        let a = (addr & 0xFFFF) as u16;
        match a {
            0x00F0 => { /* TEST: timing/waitstate control we don't model */ }
            0x00F1 => self.state.write_control(value),
            0x00F2 => self.state.dsp.set_addr(value),
            0x00F3 => {
                // Record the *selected* DSP register before the write so
                // a `DspRegisterWrite` event breakpoint can match it. The
                // S-DSP address register does not auto-increment, so the
                // index is stable across the write.
                if self.state.dsp_event_armed {
                    self.state
                        .dsp_write_events
                        .push(self.state.dsp.addr() & 0x7F);
                }
                self.state.dsp.write_data(value);
            }
            0x00F4..=0x00F7 => self.state.ports_apu_to_cpu[(a - 0x00F4) as usize] = value,
            0x00F8 => self.state.aux[0] = value,
            0x00F9 => self.state.aux[1] = value,
            0x00FA => self.state.timer0.set_target(value),
            0x00FB => self.state.timer1.set_target(value),
            0x00FC => self.state.timer2.set_target(value),
            0x00FD..=0x00FF => { /* timer outputs are read-only */ }
            // ARAM, including writes that fall under the IPL window,
            // the ROM shadows reads but the underlying ARAM is still
            // written.
            _ => self.state.aram[a as usize] = value,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> ApuState {
        ApuState::new()
    }

    #[test]
    fn ipl_window_reads_rom_when_enabled() {
        let mut s = state();
        let mut bus = ApuBus { state: &mut s };
        assert_eq!(bus.read(0xFFC0), IPL_ROM[0]);
        assert_eq!(bus.read(0xFFFF), IPL_ROM[63]);
    }

    #[test]
    fn ipl_window_reads_aram_when_disabled() {
        let mut s = state();
        s.aram[0xFFC0] = 0x5A;
        s.control = 0; // IPL disabled
        let mut bus = ApuBus { state: &mut s };
        assert_eq!(bus.read(0xFFC0), 0x5A);
    }

    #[test]
    fn writes_under_ipl_window_hit_aram() {
        let mut s = state();
        {
            let mut bus = ApuBus { state: &mut s };
            bus.write(0xFFC0, 0x77);
            // Read still shadows the ROM while enabled.
            assert_eq!(bus.read(0xFFC0), IPL_ROM[0]);
        }
        assert_eq!(s.aram[0xFFC0], 0x77);
    }

    #[test]
    fn cpu_to_apu_ports_are_read_only_to_the_spc() {
        let mut s = state();
        s.ports_cpu_to_apu = [0x11, 0x22, 0x33, 0x44];
        let mut bus = ApuBus { state: &mut s };
        assert_eq!(bus.read(0x00F4), 0x11);
        assert_eq!(bus.read(0x00F7), 0x44);
        // A SPC write to $F4-$F7 lands in the *other* direction.
        bus.write(0x00F4, 0xEE);
        assert_eq!(bus.read(0x00F4), 0x11); // unchanged input latch
        assert_eq!(bus.state.ports_apu_to_cpu[0], 0xEE);
    }

    #[test]
    fn aux_registers_round_trip() {
        let mut s = state();
        let mut bus = ApuBus { state: &mut s };
        bus.write(0x00F8, 0xA1);
        bus.write(0x00F9, 0xB2);
        assert_eq!(bus.read(0x00F8), 0xA1);
        assert_eq!(bus.read(0x00F9), 0xB2);
    }

    #[test]
    fn control_bit4_clears_input_ports_0_and_1() {
        let mut s = state();
        s.ports_cpu_to_apu = [0x11, 0x22, 0x33, 0x44];
        let mut bus = ApuBus { state: &mut s };
        bus.write(0x00F1, 0x10);
        assert_eq!(bus.state.ports_cpu_to_apu, [0, 0, 0x33, 0x44]);
    }

    #[test]
    fn control_bit5_clears_input_ports_2_and_3() {
        let mut s = state();
        s.ports_cpu_to_apu = [0x11, 0x22, 0x33, 0x44];
        let mut bus = ApuBus { state: &mut s };
        bus.write(0x00F1, 0x20);
        assert_eq!(bus.state.ports_cpu_to_apu, [0x11, 0x22, 0, 0]);
    }

    #[test]
    fn control_enables_timers_and_keeps_ipl_bit() {
        let mut s = state();
        let mut bus = ApuBus { state: &mut s };
        bus.write(0x00F1, 0x81); // IPL + timer0
        assert!(bus.state.timer0.enabled());
        assert!(!bus.state.timer1.enabled());
        assert!(bus.state.ipl_enabled());
    }

    #[test]
    fn dsp_addr_data_through_bus() {
        let mut s = state();
        let mut bus = ApuBus { state: &mut s };
        bus.write(0x00F2, 0x10);
        bus.write(0x00F3, 0xC3);
        assert_eq!(bus.read(0x00F2), 0x10);
        assert_eq!(bus.read(0x00F3), 0xC3);
    }
}
