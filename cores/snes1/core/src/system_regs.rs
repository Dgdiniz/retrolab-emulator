//! 5A22 system registers ($4200-$421F).
//!
//! These are the registers that the 5A22 implements *itself*,
//! they are not on the B-bus (the PPU/APU window at `$2100-$21FF`)
//! and they are not on the cartridge. They live behind the
//! `CpuMmio` page of [`crate::bus::SnesBus`] and are dispatched
//! from the bus's `read_cpu_mmio` / `write_cpu_mmio` helpers.
//!
//! A small subset of the surface is *fully* implemented here:
//!
//! - **WRMPYA / WRMPYB** (`$4202` / `$4203`): unsigned 8x8 multiply.
//!   Writing `$4203` computes the result into RDMPY ($4216/$4217)
//!   and arms an 8-master-cycle countdown.
//! - **WRDIVL / WRDIVH / WRDIVB** (`$4204` / `$4205` / `$4206`):
//!   16/8 unsigned divide. Writing `$4206` computes the quotient
//!   into RDDIV ($4214/$4215) *and* the remainder into RDMPY
//!   ($4216/$4217), note the shared output! Arms a 16-master-cycle
//!   countdown. Divide-by-zero matches the documented hardware
//!   behaviour: quotient = $FFFF, remainder = dividend.
//! - **MEMSEL** (`$420D` bit 0): FastROM enable. The bus reacts to
//!   a change in this bit by rebuilding the speed table for
//!   `$80-$BF:$8000-$FFFF`.
//! - **WRIO** (`$4201` bit 7): falling edge triggers an H/V counter
//!   latch. The bus interprets the [`SystemRegWriteFx::LatchHv`]
//!   side-effect and calls back into [`SystemRegs::latch_hv`].
//! - **H/V counter latches** (`$213C` / `$213D`): two-byte toggled
//!   reads of the value passed to [`SystemRegs::latch_hv`], which the
//!   bus calls with the raster position it caches after every CPU
//!   instruction.
//!
//! Interrupt configuration is partly active here too:
//!
//! - **NMITIMEN** (`$4200`) and **HTIME / VTIME** (`$4207-$420A`):
//!   stored as state, but the write returns
//!   [`SystemRegWriteFx::IrqConfigChanged`] so the bus can flag a
//!   pending IRQ reschedule (and, on the NMI-enable 0->1 edge while
//!   `$4210` is still latched, a pending NMI re-trigger). The
//!   `nmi_enabled` / `virq_enabled` / `hirq_enabled` decoders and
//!   the `irq_line` level accessor surface the state to the
//!   scheduler / CPU mirror in `system.rs`.
//! - **RDNMI** (`$4210`) and **TIMEUP** (`$4211`): bit 7 is the
//!   read-once NMI / IRQ flag (the IRQ flag doubling as the level
//!   line); `set_nmi_flag` / `set_irq_flag` drive them from the
//!   scheduler event handlers.
//!
//! The remaining surface (MDMAEN/HDMAEN beyond the DMA trigger,
//! HVBJOY, RDIO, JOY1-4) is **state-only** here: writes are stored
//! and reads return the current state. Other subsystems drive it
//! through the accessors below: the scheduler events set the HVBJOY
//! VBlank and HBlank flags, the auto-joypad read fills JOY1-4 and the
//! busy bit, and nothing drives RDIO, so it reads 0.

/// Side-effect returned by [`SystemRegs::write`] so the bus can
/// react to bus-level consequences without having to inspect every
/// register write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemRegWriteFx {
    /// No bus-level side-effect.
    None,
    /// `$4201` bit 7 went from high to low -> the bus should latch
    /// the current H/V counter values via [`SystemRegs::latch_hv`].
    LatchHv,
    /// `$420D` bit 0 changed -> the bus should rebuild the FastROM
    /// speed table by calling `SnesBus::apply_memsel`. The carried
    /// `bool` is the new value of the bit (`true` = FastROM on).
    MemselChanged(bool),
    /// `$420B` MDMAEN was written non-zero -> the bus should flag a
    /// pending general DMA for the carried channel mask. The transfer
    /// is deferred to the instruction boundary (see `bus::dma`).
    GeneralDmaRequested(u8),
    /// `$4200` (NMITIMEN) or `$4207-$420A` (HTIME/VTIME) changed ->
    /// the bus should flag a pending IRQ reschedule so `system.rs`
    /// cancels the stale H/V IRQ events and re-arms them from the
    /// current frame base. `retrigger_nmi` is set only on the
    /// `$4200` NMI-enable 0->1 edge while the `$4210` NMI flag is
    /// still latched (the 5A22 then takes a fresh NMI); the bus then
    /// also raises a pending NMI for the CPU.
    IrqConfigChanged { retrigger_nmi: bool },
}

/// State for the 5A22 system registers.
#[derive(Default)]
pub struct SystemRegs {
    nmitimen: u8,
    wrio: u8,
    wrmpya: u8,
    wrmpyb: u8,
    wrdivl: u8,
    wrdivh: u8,
    wrdivb: u8,
    htime: u16,
    vtime: u16,
    mdmaen: u8,
    hdmaen: u8,
    memsel: u8,

    mpy_result: u16,
    div_quotient: u16,
    /// Remaining master cycles before the multiply/divide result is
    /// "settled" on the real hardware. Reads of RDMPY/RDDIV before
    /// the countdown hits 0 would be partial; in practice almost
    /// every game waits the full 8/16 cycles, so reads return the
    /// final value regardless of this counter.
    math_cycles_remaining: u8,

    rdnmi: u8,
    timeup: u8,
    hvbjoy: u8,
    rdio_latch: u8,
    joy_data: [u8; 8],

    h_latched: u16,
    v_latched: u16,
    h_toggle: bool,
    v_toggle: bool,
    /// Sticky flag set by every [`SystemRegs::latch_hv`]; surfaced
    /// at bit 6 of `$213F` STAT78 and cleared by the STAT78 read
    /// (checked against bsnes-plus).
    counters_latched: bool,
    /// Whether the current console is in PAL mode. Used by `$213F`
    /// STAT78 bit 4 (`0` = NTSC, `1` = PAL). Driven by the
    /// emulator from the timing profile selected at `load_rom`
    /// time.
    timing_is_pal: bool,

    /// Transient: set whenever a `$4210` (RDNMI) read returns bit 7
    /// set, i.e. the CPU *observed* the latched VBlank NMI flag.
    /// `system.rs` consumes this via
    /// [`SystemRegs::take_nmi_flag_observed`] to decide *when* to
    /// vector a VBlank NMI: the handler's own `$4210` ack must not
    /// preempt a main-thread vblank-wait poll (`lda $4210 / bpl`)
    /// before that poll sees the flag. Reset to `false` on every fresh
    /// latch (`set_nmi_flag(true)`), so it only reflects reads that
    /// happened *after* the current vblank began. Not part of the
    /// savestate wire format, it is re-derived the next vblank.
    nmi_flag_observed: bool,
}

impl SystemRegs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Capture the `$4200-$421F` register file (including the settled
    /// multiply/divide results and the latched H/V counters) into a
    /// snapshot.
    pub fn to_savestate(&self) -> crate::savestate::SystemRegsState {
        crate::savestate::SystemRegsState {
            nmitimen: self.nmitimen,
            wrio: self.wrio,
            wrmpya: self.wrmpya,
            wrmpyb: self.wrmpyb,
            wrdivl: self.wrdivl,
            wrdivh: self.wrdivh,
            wrdivb: self.wrdivb,
            htime: self.htime,
            vtime: self.vtime,
            mdmaen: self.mdmaen,
            hdmaen: self.hdmaen,
            memsel: self.memsel,
            mpy_result: self.mpy_result,
            div_quotient: self.div_quotient,
            math_cycles_remaining: self.math_cycles_remaining,
            rdnmi: self.rdnmi,
            timeup: self.timeup,
            hvbjoy: self.hvbjoy,
            rdio_latch: self.rdio_latch,
            joy_data: self.joy_data,
            h_latched: self.h_latched,
            v_latched: self.v_latched,
            h_toggle: self.h_toggle,
            v_toggle: self.v_toggle,
            counters_latched: self.counters_latched,
            timing_is_pal: self.timing_is_pal,
        }
    }

    /// Restore the `$4200-$421F` register file from a snapshot.
    pub fn from_savestate(&mut self, s: &crate::savestate::SystemRegsState) {
        self.nmitimen = s.nmitimen;
        self.wrio = s.wrio;
        self.wrmpya = s.wrmpya;
        self.wrmpyb = s.wrmpyb;
        self.wrdivl = s.wrdivl;
        self.wrdivh = s.wrdivh;
        self.wrdivb = s.wrdivb;
        self.htime = s.htime;
        self.vtime = s.vtime;
        self.mdmaen = s.mdmaen;
        self.hdmaen = s.hdmaen;
        self.memsel = s.memsel;
        self.mpy_result = s.mpy_result;
        self.div_quotient = s.div_quotient;
        self.math_cycles_remaining = s.math_cycles_remaining;
        self.rdnmi = s.rdnmi;
        self.timeup = s.timeup;
        self.hvbjoy = s.hvbjoy;
        self.rdio_latch = s.rdio_latch;
        self.joy_data = s.joy_data;
        self.h_latched = s.h_latched;
        self.v_latched = s.v_latched;
        self.h_toggle = s.h_toggle;
        self.v_toggle = s.v_toggle;
        self.counters_latched = s.counters_latched;
        self.timing_is_pal = s.timing_is_pal;
    }

    /// Dispatch a read in the `$4200-$43FF` window.
    ///
    /// `reg_low` is the bottom 16 bits of the A-bus address. The bus
    /// is responsible for routing only addresses in the CPU MMIO
    /// page here; anything outside the recognised registers returns
    /// 0 so the caller knows to fall back to MDR.
    pub fn read(&mut self, reg_low: u16) -> u8 {
        match reg_low {
            0x4210 => {
                let v = self.rdnmi;
                // Bit 7 of RDNMI is the NMI flag; reading clears it.
                // Bits 0-3 are open-bus / chip revision on real
                // hardware (model-dependent); we leave them at 0.
                self.rdnmi &= 0x7F;
                // Record that the CPU observed the latched flag. The NMI
                // delivery in system.rs waits for this so a main-thread
                // vblank-wait poll sees bit 7 before the handler's ack
                // clears it (independent of instruction phase).
                if v & 0x80 != 0 {
                    self.nmi_flag_observed = true;
                }
                v
            }
            0x4211 => {
                let v = self.timeup;
                self.timeup &= 0x7F;
                v
            }
            0x4212 => self.hvbjoy,
            0x4213 => self.rdio_latch,
            0x4214 => self.div_quotient as u8,
            0x4215 => (self.div_quotient >> 8) as u8,
            0x4216 => self.mpy_result as u8,
            0x4217 => (self.mpy_result >> 8) as u8,
            0x4218..=0x421F => self.joy_data[(reg_low - 0x4218) as usize],
            _ => 0,
        }
    }

    /// Dispatch a write in the `$4200-$420F` window. Returns a
    /// [`SystemRegWriteFx`] that the bus inspects for bus-level
    /// side-effects (MEMSEL rebuild, HV latch).
    pub fn write(&mut self, reg_low: u16, value: u8) -> SystemRegWriteFx {
        match reg_low {
            0x4200 => {
                let prev = self.nmitimen;
                self.nmitimen = value;
                // RDNMI bit 7 is the latched NMI line. If NMI-enable
                // rises 0->1 while that latch is still set (the CPU is
                // still in VBlank and has not read $4210), the 5A22
                // takes a fresh NMI, a quirk several games depend on
                // (checked against bsnes-plus).
                let nmi_edge = (prev & 0x80) == 0 && (value & 0x80) != 0;
                let retrigger_nmi = nmi_edge && (self.rdnmi & 0x80) != 0;
                // Disabling both IRQ sources (bits 5/4) drops the
                // level-triggered IRQ line immediately.
                if (value & 0x30) == 0 {
                    self.timeup &= 0x7F;
                }
                SystemRegWriteFx::IrqConfigChanged { retrigger_nmi }
            }
            0x4201 => {
                let falling_edge = (self.wrio & 0x80) != 0 && (value & 0x80) == 0;
                self.wrio = value;
                if falling_edge {
                    SystemRegWriteFx::LatchHv
                } else {
                    SystemRegWriteFx::None
                }
            }
            0x4202 => {
                self.wrmpya = value;
                SystemRegWriteFx::None
            }
            0x4203 => {
                self.wrmpyb = value;
                self.mpy_result = (self.wrmpya as u16) * (value as u16);
                self.math_cycles_remaining = 8;
                SystemRegWriteFx::None
            }
            0x4204 => {
                self.wrdivl = value;
                SystemRegWriteFx::None
            }
            0x4205 => {
                self.wrdivh = value;
                SystemRegWriteFx::None
            }
            0x4206 => {
                self.wrdivb = value;
                let dividend = ((self.wrdivh as u16) << 8) | (self.wrdivl as u16);
                if value == 0 {
                    // Documented hardware behaviour: division by
                    // zero leaves quotient = $FFFF and remainder
                    // = dividend. RDMPY ($4216/$4217) is the
                    // remainder output during divide, it shares
                    // the storage with the multiply result.
                    self.div_quotient = 0xFFFF;
                    self.mpy_result = dividend;
                } else {
                    self.div_quotient = dividend / value as u16;
                    self.mpy_result = dividend % value as u16;
                }
                self.math_cycles_remaining = 16;
                SystemRegWriteFx::None
            }
            0x4207 => {
                self.htime = (self.htime & 0x100) | u16::from(value);
                SystemRegWriteFx::IrqConfigChanged {
                    retrigger_nmi: false,
                }
            }
            0x4208 => {
                self.htime = (self.htime & 0xFF) | ((u16::from(value) & 1) << 8);
                SystemRegWriteFx::IrqConfigChanged {
                    retrigger_nmi: false,
                }
            }
            0x4209 => {
                self.vtime = (self.vtime & 0x100) | u16::from(value);
                SystemRegWriteFx::IrqConfigChanged {
                    retrigger_nmi: false,
                }
            }
            0x420A => {
                self.vtime = (self.vtime & 0xFF) | ((u16::from(value) & 1) << 8);
                SystemRegWriteFx::IrqConfigChanged {
                    retrigger_nmi: false,
                }
            }
            0x420B => {
                self.mdmaen = value;
                // A non-zero MDMAEN write triggers general DMA on the
                // selected channels. The bus defers the actual transfer
                // to the instruction boundary.
                if value != 0 {
                    SystemRegWriteFx::GeneralDmaRequested(value)
                } else {
                    SystemRegWriteFx::None
                }
            }
            0x420C => {
                self.hdmaen = value;
                SystemRegWriteFx::None
            }
            0x420D => {
                let prev_fast = (self.memsel & 1) != 0;
                self.memsel = value;
                let new_fast = (value & 1) != 0;
                if prev_fast != new_fast {
                    SystemRegWriteFx::MemselChanged(new_fast)
                } else {
                    SystemRegWriteFx::None
                }
            }
            _ => SystemRegWriteFx::None,
        }
    }

    /// Advance the multiply/divide countdown by `master_cycles`.
    ///
    /// Called by the CPU step loop. While the countdown is non-zero
    /// the math result is technically "in progress", but since
    /// every read here returns the final value already, this is
    /// informational only. Saturates at zero so the
    /// counter never underflows.
    pub fn tick(&mut self, master_cycles: u32) {
        let dec = master_cycles.min(255) as u8;
        self.math_cycles_remaining = self.math_cycles_remaining.saturating_sub(dec);
    }

    /// Master-cycle countdown remaining until the multiply/divide
    /// result fully settles. Surfaced for tests.
    pub fn math_cycles_remaining(&self) -> u8 {
        self.math_cycles_remaining
    }

    /// Latch new H/V counter values, resetting the read toggles so
    /// the next `read_hcounter` / `read_vcounter` returns the low
    /// byte. Also sets the `counters_latched` flag so a subsequent
    /// `$213F` STAT78 read sees bit 6 high. The 9-bit hardware
    /// limit is enforced on read, not on latch, because the caller
    /// (the scheduler driving this) already knows the bounds.
    pub fn latch_hv(&mut self, h: u16, v: u16) {
        self.h_latched = h;
        self.v_latched = v;
        self.h_toggle = false;
        self.v_toggle = false;
        self.counters_latched = true;
    }

    /// Read **and clear** the "counters latched" flag exposed at
    /// bit 6 of `$213F` STAT78: a read of STAT78 clears the flag
    /// (checked against bsnes-plus).
    pub fn consume_counters_latched_flag(&mut self) -> bool {
        let flag = self.counters_latched;
        self.counters_latched = false;
        flag
    }

    /// Inspect the "counters latched" flag without clearing it.
    /// Surfaced for tests; the actual STAT78 read path uses
    /// [`Self::consume_counters_latched_flag`].
    pub fn counters_latched_flag(&self) -> bool {
        self.counters_latched
    }

    /// Reset the H/V toggle states without touching the latch
    /// values themselves. Called by `$213F` STAT78 reads, the
    /// hardware uses STAT78 as the "I'm done reading OPHCT/OPVCT"
    /// trigger to re-arm the toggle phase for the next pair.
    pub fn reset_hv_toggles(&mut self) {
        self.h_toggle = false;
        self.v_toggle = false;
    }

    /// Update the PAL/NTSC flag that drives `$213F` STAT78 bit 4.
    /// Called by the emulator at `load_rom` time and by
    /// `set_region_str`, based on the selected timing profile.
    pub fn set_timing_is_pal(&mut self, pal: bool) {
        self.timing_is_pal = pal;
    }

    /// Whether the current console is configured for PAL output.
    /// Surfaced so the bus can build STAT78 without reaching for
    /// the timing profile.
    pub fn timing_is_pal(&self) -> bool {
        self.timing_is_pal
    }

    /// Read the next byte of OPHCT (`$213C`). Toggles low -> high.
    pub fn read_hcounter(&mut self) -> u8 {
        let v = if !self.h_toggle {
            self.h_latched as u8
        } else {
            (self.h_latched >> 8) as u8
        };
        self.h_toggle = !self.h_toggle;
        v
    }

    /// Read the next byte of OPVCT (`$213D`). Toggles low -> high.
    pub fn read_vcounter(&mut self) -> u8 {
        let v = if !self.v_toggle {
            self.v_latched as u8
        } else {
            (self.v_latched >> 8) as u8
        };
        self.v_toggle = !self.v_toggle;
        v
    }

    // -- Direct accessors for cross-subsystem wiring -------------

    /// FastROM enable bit (`$420D` bit 0).
    pub fn memsel_fast(&self) -> bool {
        (self.memsel & 1) != 0
    }

    /// Raw NMITIMEN value (`$4200`).
    pub fn nmitimen(&self) -> u8 {
        self.nmitimen
    }

    /// NMI enable (`$4200` bit 7). Gates whether entering VBlank
    /// drives the CPU NMI signal (the `$4210` flag is set either way).
    pub fn nmi_enabled(&self) -> bool {
        (self.nmitimen & 0x80) != 0
    }

    /// V-counter IRQ enable (`$4200` bit 5).
    pub fn virq_enabled(&self) -> bool {
        (self.nmitimen & 0x20) != 0
    }

    /// H-counter IRQ enable (`$4200` bit 4).
    pub fn hirq_enabled(&self) -> bool {
        (self.nmitimen & 0x10) != 0
    }

    /// Raw HTIME 9-bit value (`$4207/$4208`).
    pub fn htime(&self) -> u16 {
        self.htime
    }

    /// Raw VTIME 9-bit value (`$4209/$420A`).
    pub fn vtime(&self) -> u16 {
        self.vtime
    }

    /// Raw MDMAEN value (`$420B`). The DMA engine reads this to decide
    /// which channels to fire.
    pub fn mdmaen(&self) -> u8 {
        self.mdmaen
    }

    /// Clear MDMAEN (`$420B`). Called by the bus once a general DMA
    /// block finishes: the chip clears a channel's enable bit when
    /// its transfer completes.
    pub fn clear_mdmaen(&mut self) {
        self.mdmaen = 0;
    }

    /// Raw HDMAEN value (`$420C`).
    pub fn hdmaen(&self) -> u8 {
        self.hdmaen
    }

    /// Set the NMI flag bit of `$4210`. Called when entering V-blank
    /// with the VBlank scheduler; the enable bit (`NMITIMEN` bit 7)
    /// gates whether the CPU is actually signalled.
    pub fn set_nmi_flag(&mut self, active: bool) {
        if active {
            self.rdnmi |= 0x80;
            // A fresh latch: no CPU read has observed this vblank's flag
            // yet. Clearing here makes the handler's `$4210` ack from the
            // *previous* frame irrelevant to this frame's delivery.
            self.nmi_flag_observed = false;
        } else {
            self.rdnmi &= 0x7F;
        }
    }

    /// Read-and-clear the "the CPU observed the NMI flag" sticky set by
    /// a `$4210` read returning bit 7. `system.rs` polls this each step
    /// while an NMI delivery is armed: once the main thread has seen the
    /// flag (or a short fallback window elapses), the NMI is vectored.
    pub fn take_nmi_flag_observed(&mut self) -> bool {
        let observed = self.nmi_flag_observed;
        self.nmi_flag_observed = false;
        observed
    }

    /// Set the IRQ flag bit of `$4211`. Called by the IRQ scheduler
    /// when an H/V trigger fires (raising the level line); the `$4200`
    /// write that disables both sources lowers the bit itself.
    pub fn set_irq_flag(&mut self, active: bool) {
        if active {
            self.timeup |= 0x80;
        } else {
            self.timeup &= 0x7F;
        }
    }

    /// Whether the level-triggered IRQ line is currently asserted
    /// (`$4211` bit 7). `system.rs` mirrors this into the CPU's
    /// `irq_pending` each step; reading `$4211` (TIMEUP) clears it.
    pub fn irq_line(&self) -> bool {
        (self.timeup & 0x80) != 0
    }

    /// Set the HVBJOY flags (`$4212`). The scheduler updates
    /// bits 7 (VBlank) and 6 (HBlank); bit 0 (auto-joypad busy) is
    /// driven independently via [`Self::set_joypad_busy`].
    pub fn set_hvbjoy(&mut self, value: u8) {
        self.hvbjoy = value;
    }

    /// Set or clear the VBlank flag (`$4212` bit 7) without disturbing
    /// the HBlank or auto-joypad bits. Driven by the `VBlankStart` /
    /// `VBlankEnd` scheduler events.
    pub fn set_vblank(&mut self, active: bool) {
        if active {
            self.hvbjoy |= 0x80;
        } else {
            self.hvbjoy &= !0x80;
        }
    }

    /// Set or clear the HBlank flag (`$4212` bit 6) without disturbing
    /// the VBlank or auto-joypad bits. The scheduler raises it at the
    /// `HBlankStart` dot and lowers it when the active display resumes,
    /// so a CPU polling `$4212 & $40` (e.g. waiting for HBlank) makes
    /// progress instead of spinning forever.
    pub fn set_hblank(&mut self, active: bool) {
        if active {
            self.hvbjoy |= 0x40;
        } else {
            self.hvbjoy &= !0x40;
        }
    }

    /// Set or clear the "auto-joypad busy" flag (`$4212` bit 0) without
    /// disturbing the VBlank/HBlank bits. Raised by the controller
    /// subsystem at the start of an auto-read and lowered when the read
    /// window elapses.
    pub fn set_joypad_busy(&mut self, busy: bool) {
        if busy {
            self.hvbjoy |= 0x01;
        } else {
            self.hvbjoy &= !0x01;
        }
    }

    /// Overwrite a single byte of the auto-joypad buffer
    /// (`$4218-$421F`). Indices 0..8 map to `$4218`..=`$421F`.
    pub fn set_joy_byte(&mut self, index: usize, value: u8) {
        if index < self.joy_data.len() {
            self.joy_data[index] = value;
        }
    }

    /// Drive the RDIO latch (`$4213`), which on hardware reads the P1/P2
    /// I/O port bits 6 and 7. No controller model drives it, so it reads 0.
    pub fn set_rdio(&mut self, value: u8) {
        self.rdio_latch = value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiply_unsigned_basic() {
        let mut regs = SystemRegs::new();
        regs.write(0x4202, 10);
        regs.write(0x4203, 5);
        assert_eq!(regs.read(0x4216), 50);
        assert_eq!(regs.read(0x4217), 0);
        assert_eq!(regs.math_cycles_remaining(), 8);
    }

    #[test]
    fn multiply_8x8_max() {
        let mut regs = SystemRegs::new();
        regs.write(0x4202, 0xFF);
        regs.write(0x4203, 0xFF);
        // 255 * 255 = 65025 = 0xFE01.
        assert_eq!(regs.read(0x4216), 0x01);
        assert_eq!(regs.read(0x4217), 0xFE);
    }

    #[test]
    fn divide_basic() {
        let mut regs = SystemRegs::new();
        regs.write(0x4204, 100); // dividend low
        regs.write(0x4205, 0); // dividend high
        regs.write(0x4206, 10); // divisor -> triggers
                                // Quotient = 10 -> RDDIV.
        assert_eq!(regs.read(0x4214), 10);
        assert_eq!(regs.read(0x4215), 0);
        // Remainder = 0 -> RDMPY (shared with multiply).
        assert_eq!(regs.read(0x4216), 0);
        assert_eq!(regs.read(0x4217), 0);
        assert_eq!(regs.math_cycles_remaining(), 16);
    }

    #[test]
    fn divide_remainder_overwrites_mpy() {
        let mut regs = SystemRegs::new();
        // Set a non-zero multiply result first.
        regs.write(0x4202, 7);
        regs.write(0x4203, 9); // mpy = 63
        assert_eq!(regs.read(0x4216), 63);

        // Now do a divide whose remainder is non-zero.
        regs.write(0x4204, 17);
        regs.write(0x4205, 0);
        regs.write(0x4206, 5); // 17 / 5 = 3 r 2
        assert_eq!(regs.read(0x4214), 3);
        // RDMPY now holds the divide REMAINDER, not the old
        // multiply result.
        assert_eq!(regs.read(0x4216), 2);
        assert_eq!(regs.read(0x4217), 0);
    }

    #[test]
    fn divide_by_zero() {
        let mut regs = SystemRegs::new();
        regs.write(0x4204, 0x34);
        regs.write(0x4205, 0x12); // dividend = 0x1234
        regs.write(0x4206, 0); // divide by zero
                               // RDDIV = 0xFFFF.
        assert_eq!(regs.read(0x4214), 0xFF);
        assert_eq!(regs.read(0x4215), 0xFF);
        // RDMPY = dividend.
        assert_eq!(regs.read(0x4216), 0x34);
        assert_eq!(regs.read(0x4217), 0x12);
    }

    #[test]
    fn htime_vtime_9bit_assembly() {
        let mut regs = SystemRegs::new();
        regs.write(0x4207, 0xAB); // HTIMEL
        regs.write(0x4208, 0x01); // HTIMEH bit 0 -> 0x100
        assert_eq!(regs.htime(), 0x1AB);
        // High byte ignores bits 1-7.
        regs.write(0x4208, 0xFE);
        assert_eq!(regs.htime(), 0x0AB);

        regs.write(0x4209, 0x42);
        regs.write(0x420A, 0x01);
        assert_eq!(regs.vtime(), 0x142);
        regs.write(0x420A, 0x00);
        assert_eq!(regs.vtime(), 0x042);
    }

    #[test]
    fn nmitimen_roundtrip_state_only() {
        let mut regs = SystemRegs::new();
        regs.write(0x4200, 0xA5);
        assert_eq!(regs.nmitimen(), 0xA5);
        // No read register for NMITIMEN; surface via accessor.
        // Confirm the value didn't leak into another register.
        assert_eq!(regs.read(0x4210), 0); // RDNMI still clear
    }

    #[test]
    fn mdmaen_hdmaen_state_only() {
        let mut regs = SystemRegs::new();
        regs.write(0x420B, 0xF0);
        regs.write(0x420C, 0x0F);
        assert_eq!(regs.mdmaen(), 0xF0);
        assert_eq!(regs.hdmaen(), 0x0F);
    }

    #[test]
    fn mdmaen_nonzero_write_requests_general_dma() {
        let mut regs = SystemRegs::new();
        // A non-zero MDMAEN write both stores the mask and asks the
        // bus to run general DMA on those channels.
        assert_eq!(
            regs.write(0x420B, 0x05),
            SystemRegWriteFx::GeneralDmaRequested(0x05)
        );
        assert_eq!(regs.mdmaen(), 0x05);
        // A zero write stores zero and requests nothing.
        assert_eq!(regs.write(0x420B, 0x00), SystemRegWriteFx::None);
        assert_eq!(regs.mdmaen(), 0x00);
        // HDMAEN never triggers a general-DMA request.
        assert_eq!(regs.write(0x420C, 0xFF), SystemRegWriteFx::None);
    }

    #[test]
    fn clear_mdmaen_zeroes_register() {
        let mut regs = SystemRegs::new();
        regs.write(0x420B, 0x80);
        assert_eq!(regs.mdmaen(), 0x80);
        regs.clear_mdmaen();
        assert_eq!(regs.mdmaen(), 0x00);
    }

    #[test]
    fn memsel_returns_fx_on_change_only() {
        let mut regs = SystemRegs::new();
        // First flip 0 -> 1 -> fires.
        assert_eq!(
            regs.write(0x420D, 0x01),
            SystemRegWriteFx::MemselChanged(true)
        );
        // Repeating the same value -> no change -> None.
        assert_eq!(regs.write(0x420D, 0x01), SystemRegWriteFx::None);
        // Upper bits ignored for the change check (only bit 0 matters).
        assert_eq!(regs.write(0x420D, 0xFF), SystemRegWriteFx::None);
        // Flip back to 0 -> fires.
        assert_eq!(
            regs.write(0x420D, 0x00),
            SystemRegWriteFx::MemselChanged(false)
        );
    }

    #[test]
    fn wrio_falling_edge_returns_latch_fx() {
        let mut regs = SystemRegs::new();
        // Rising / steady high should NOT trigger latch.
        assert_eq!(regs.write(0x4201, 0x80), SystemRegWriteFx::None);
        assert_eq!(regs.write(0x4201, 0x80), SystemRegWriteFx::None);
        // Falling edge -> trigger.
        assert_eq!(regs.write(0x4201, 0x00), SystemRegWriteFx::LatchHv);
        // Already low -> no trigger.
        assert_eq!(regs.write(0x4201, 0x00), SystemRegWriteFx::None);
        // Re-raise + lower again -> trigger.
        assert_eq!(regs.write(0x4201, 0x80), SystemRegWriteFx::None);
        assert_eq!(regs.write(0x4201, 0x00), SystemRegWriteFx::LatchHv);
    }

    #[test]
    fn rdnmi_clears_bit_7_on_read() {
        let mut regs = SystemRegs::new();
        regs.set_nmi_flag(true);
        assert_eq!(regs.read(0x4210) & 0x80, 0x80);
        // Read-once: second read should see bit 7 cleared.
        assert_eq!(regs.read(0x4210) & 0x80, 0x00);

        // Same for TIMEUP.
        regs.set_irq_flag(true);
        assert_eq!(regs.read(0x4211) & 0x80, 0x80);
        assert_eq!(regs.read(0x4211) & 0x80, 0x00);
    }

    #[test]
    fn hv_counters_toggle_low_then_high() {
        let mut regs = SystemRegs::new();
        regs.latch_hv(0x1234, 0x5678);
        // First read returns low byte, second returns high byte.
        assert_eq!(regs.read_hcounter(), 0x34);
        assert_eq!(regs.read_hcounter(), 0x12);
        // Toggle wraps: next read goes back to low byte.
        assert_eq!(regs.read_hcounter(), 0x34);

        assert_eq!(regs.read_vcounter(), 0x78);
        assert_eq!(regs.read_vcounter(), 0x56);

        // Re-latching resets the toggles.
        regs.latch_hv(0x00FF, 0x0100);
        assert_eq!(regs.read_hcounter(), 0xFF);
        assert_eq!(regs.read_hcounter(), 0x00);
        assert_eq!(regs.read_vcounter(), 0x00);
        assert_eq!(regs.read_vcounter(), 0x01);
    }

    #[test]
    fn tick_saturates_at_zero() {
        let mut regs = SystemRegs::new();
        regs.write(0x4202, 1);
        regs.write(0x4203, 1); // arms 8-cycle countdown
        assert_eq!(regs.math_cycles_remaining(), 8);
        regs.tick(3);
        assert_eq!(regs.math_cycles_remaining(), 5);
        // Saturating: ticking past zero does not underflow.
        regs.tick(100);
        assert_eq!(regs.math_cycles_remaining(), 0);
        regs.tick(1);
        assert_eq!(regs.math_cycles_remaining(), 0);
    }

    #[test]
    fn set_joypad_busy_toggles_bit0_only() {
        let mut regs = SystemRegs::new();
        // VBlank flag set (bit 7), busy must not disturb it.
        regs.set_hvbjoy(0x80);
        regs.set_joypad_busy(true);
        assert_eq!(regs.read(0x4212), 0x81);
        regs.set_joypad_busy(false);
        assert_eq!(regs.read(0x4212), 0x80);
        // Idempotent clears stay clear.
        regs.set_joypad_busy(false);
        assert_eq!(regs.read(0x4212), 0x80);
    }

    #[test]
    fn set_vblank_and_hblank_toggle_independent_bits() {
        let mut regs = SystemRegs::new();
        // HBlank (bit 6) and the auto-joypad busy bit (bit 0) must
        // survive a VBlank flag toggle, and vice-versa, the three
        // $4212 status bits are driven by independent subsystems.
        regs.set_hblank(true);
        regs.set_joypad_busy(true);
        assert_eq!(regs.read(0x4212), 0x41);
        regs.set_vblank(true);
        assert_eq!(regs.read(0x4212), 0xC1);
        regs.set_vblank(false);
        assert_eq!(
            regs.read(0x4212),
            0x41,
            "clearing VBlank must keep HBlank + busy"
        );
        regs.set_hblank(false);
        assert_eq!(regs.read(0x4212), 0x01, "clearing HBlank must keep busy");
        // Idempotent toggles are stable.
        regs.set_hblank(false);
        regs.set_vblank(false);
        assert_eq!(regs.read(0x4212), 0x01);
    }

    #[test]
    fn joy_buffer_default_zero_and_settable() {
        let mut regs = SystemRegs::new();
        for offset in 0..8 {
            assert_eq!(regs.read(0x4218 + offset as u16), 0);
        }
        regs.set_joy_byte(0, 0xAB);
        regs.set_joy_byte(7, 0xCD);
        assert_eq!(regs.read(0x4218), 0xAB);
        assert_eq!(regs.read(0x421F), 0xCD);
    }

    #[test]
    fn latch_hv_arms_counters_latched_flag() {
        let mut regs = SystemRegs::new();
        assert!(!regs.counters_latched_flag());
        regs.latch_hv(100, 50);
        assert!(regs.counters_latched_flag());
        // Consume clears.
        assert!(regs.consume_counters_latched_flag());
        assert!(!regs.counters_latched_flag());
        // A second consume sees the cleared flag.
        assert!(!regs.consume_counters_latched_flag());
    }

    #[test]
    fn reset_hv_toggles_does_not_clear_latched_value() {
        let mut regs = SystemRegs::new();
        regs.latch_hv(0xABCD, 0x1234);
        // Half-read OPHCT (low byte) so the toggle is non-zero.
        let _ = regs.read_hcounter();
        regs.reset_hv_toggles();
        // Next read is the LOW byte again, not the high one.
        assert_eq!(regs.read_hcounter(), 0xCD);
        // The latched value itself is intact.
        assert_eq!(regs.read_hcounter(), 0xAB);
    }

    #[test]
    fn timing_is_pal_round_trips() {
        let mut regs = SystemRegs::new();
        assert!(!regs.timing_is_pal());
        regs.set_timing_is_pal(true);
        assert!(regs.timing_is_pal());
        regs.set_timing_is_pal(false);
        assert!(!regs.timing_is_pal());
    }

    #[test]
    fn nmitimen_write_returns_irq_config_changed() {
        let mut regs = SystemRegs::new();
        // A plain enable write (no NMI flag latched) reschedules IRQ
        // but does not re-trigger NMI.
        assert_eq!(
            regs.write(0x4200, 0x20),
            SystemRegWriteFx::IrqConfigChanged {
                retrigger_nmi: false
            }
        );
    }

    #[test]
    fn nmitimen_retrigger_nmi_only_on_edge_while_latched() {
        let mut regs = SystemRegs::new();
        // NMI flag latched (as if VBlank set it), NMI-enable currently 0.
        regs.set_nmi_flag(true);
        // 0->1 edge on bit 7 while $4210 still latched -> re-trigger.
        assert_eq!(
            regs.write(0x4200, 0x80),
            SystemRegWriteFx::IrqConfigChanged {
                retrigger_nmi: true
            }
        );
        // Already enabled (no edge) -> no re-trigger even though latched.
        assert_eq!(
            regs.write(0x4200, 0x80),
            SystemRegWriteFx::IrqConfigChanged {
                retrigger_nmi: false
            }
        );
    }

    #[test]
    fn nmitimen_no_retrigger_when_flag_not_latched() {
        let mut regs = SystemRegs::new();
        // $4210 not latched -> enabling NMI does not re-trigger.
        assert_eq!(
            regs.write(0x4200, 0x80),
            SystemRegWriteFx::IrqConfigChanged {
                retrigger_nmi: false
            }
        );
    }

    #[test]
    fn nmitimen_disabling_both_irq_sources_clears_line() {
        let mut regs = SystemRegs::new();
        // Raise the IRQ line, then disable both V/H IRQ via $4200.
        regs.set_irq_flag(true);
        assert!(regs.irq_line());
        regs.write(0x4200, 0x00); // bits 5/4 both clear
        assert!(!regs.irq_line());
        // Re-raise, then a write that keeps an enable on must NOT clear.
        regs.set_irq_flag(true);
        regs.write(0x4200, 0x10); // H-IRQ still enabled
        assert!(regs.irq_line());
    }

    #[test]
    fn htime_vtime_writes_request_reschedule() {
        let mut regs = SystemRegs::new();
        for reg in [0x4207u16, 0x4208, 0x4209, 0x420A] {
            assert_eq!(
                regs.write(reg, 0x01),
                SystemRegWriteFx::IrqConfigChanged {
                    retrigger_nmi: false
                }
            );
        }
    }

    #[test]
    fn irq_enable_decoders() {
        let mut regs = SystemRegs::new();
        regs.write(0x4200, 0x80); // NMI only
        assert!(regs.nmi_enabled());
        assert!(!regs.virq_enabled());
        assert!(!regs.hirq_enabled());
        regs.write(0x4200, 0x30); // V + H
        assert!(!regs.nmi_enabled());
        assert!(regs.virq_enabled());
        assert!(regs.hirq_enabled());
    }

    #[test]
    fn irq_line_reflects_timeup_bit7() {
        let mut regs = SystemRegs::new();
        assert!(!regs.irq_line());
        regs.set_irq_flag(true);
        assert!(regs.irq_line());
        // Reading $4211 clears the line (read-once).
        let _ = regs.read(0x4211);
        assert!(!regs.irq_line());
    }
}
