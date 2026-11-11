//! WDC 65C816 CPU emulation.
//!
//! Implements the 65C816 processor used in the SNES (as Ricoh 5A22),
//! Apple IIGS, and other systems. Features dual-mode operation
//! (emulation/native), variable-width registers, and 24-bit addressing.

pub mod addressing;
pub mod decode;
pub mod disasm;
pub mod instructions;
pub mod interrupts;

/// SingleStepTests runner, shared by the crate's integration tests and
/// `snes1-cli singlestep-65816`. Gated behind the `singlestep` feature so the
/// default and WASM builds never compile in `serde`/`serde_json`.
#[cfg(feature = "singlestep")]
pub mod singlestep;

use decode::OPCODE_TABLE;
use retro_core::cpu::{Addressable, Clockable, Resettable};

/// WDC 65C816 CPU.
pub struct Wdc65816 {
    /// Full 16-bit accumulator. Low byte = A, high byte = B.
    pub c: u16,
    pub x: u16,
    pub y: u16,
    /// Stack pointer (16-bit in native mode, forced to $01xx in emulation)
    pub sp: u16,
    pub pc: u16,

    /// Program Bank Register (bits 16-23 of instruction fetch)
    pub pbr: u8,
    /// Data Bank Register (bits 16-23 of data access)
    pub dbr: u8,
    /// Direct Page register (replaces 6502 zero page)
    pub d: u16,

    /// Status: NVMXDIZC (native) or NV-BDIZC (emulation)
    pub p: u8,
    /// Emulation mode flag (not part of P)
    pub emulation: bool,

    pub cycles_executed: u64,

    pub nmi_pending: bool,
    pub irq_pending: bool,
    pub nmi_edge_detected: bool,
    /// WAI instruction halts until interrupt
    pub waiting: bool,
    /// STP instruction stops the clock
    pub stopped: bool,
}

impl Wdc65816 {
    pub fn new() -> Self {
        Self {
            c: 0,
            x: 0,
            y: 0,
            sp: 0x01FD,
            pc: 0,
            pbr: 0,
            dbr: 0,
            d: 0,
            p: 0x34, // M=1, X=1, I=1 in emulation
            emulation: true,
            cycles_executed: 0,
            nmi_pending: false,
            irq_pending: false,
            nmi_edge_detected: false,
            waiting: false,
            stopped: false,
        }
    }

    /// Hardware reset: reads vector from $00:FFFC and enters emulation mode.
    pub fn reset_with_bus(&mut self, bus: &mut dyn Addressable) {
        self.c = 0;
        self.x = 0;
        self.y = 0;
        self.sp = 0x01FD;
        self.pbr = 0;
        self.dbr = 0;
        self.d = 0;
        self.p = 0x34;
        self.emulation = true;
        self.waiting = false;
        self.stopped = false;
        self.nmi_pending = false;
        self.irq_pending = false;
        self.nmi_edge_detected = false;
        self.cycles_executed = 0;

        let lo = bus.read(interrupts::EMU_RESET_VECTOR as u32) as u16;
        let hi = bus.read((interrupts::EMU_RESET_VECTOR + 1) as u32) as u16;
        self.pc = (hi << 8) | lo;
    }

    /// Execute one instruction. Returns cycles consumed.
    pub fn step(&mut self, bus: &mut dyn Addressable) -> u32 {
        if self.stopped {
            return 1;
        }

        if self.waiting {
            // WAI halts the CPU until *any* interrupt line asserts, and the
            // I flag gates only whether that interrupt is then *serviced*,
            // never whether the CPU wakes. An IRQ arriving with I set still
            // pulls the CPU out of WAI; `poll_interrupts` below simply
            // declines to vector, so execution resumes at the instruction
            // after WAI. Games disable IRQs and use `WAI` purely to sync to a
            // raster trigger without paying for a handler (Star Fox / Doom's
            // 3D frame loops do exactly `SEI ... WAI`); gating the wake on the I
            // flag deadlocks them. The wake comes first and the I flag only
            // decides whether the interrupt is then serviced (checked
            // against bsnes-plus).
            if self.nmi_pending || self.irq_pending {
                self.waiting = false;
            } else {
                return 1;
            }
        }

        if let Some(cycles) = self.poll_interrupts(bus) {
            return cycles;
        }

        let opcode = self.fetch_byte(bus);
        let info = &OPCODE_TABLE[opcode as usize];

        let extra = self.execute(bus, opcode, info);

        let mut total = info.base_cycles as u32 + extra;
        if info.penalty_m16 && !self.a_is_8bit() {
            total += 1;
            if info.penalty_rmw_m16 {
                // Read-modify-write on 16-bit memory costs an extra cycle
                // on top of the standard `+1 if M=0` already added above.
                total += 1;
            }
        }
        if info.penalty_x16 && !self.xy_is_8bit() {
            total += 1;
        }
        if info.penalty_dp && (self.d & 0xFF) != 0 {
            total += 1;
        }
        if !self.emulation {
            total += info.penalty_native as u32;
        }

        self.cycles_executed += total as u64;
        total
    }

    // ---- Mode queries ----

    /// True when the accumulator operates in 8-bit mode.
    #[inline]
    pub fn a_is_8bit(&self) -> bool {
        self.emulation || (self.p & instructions::FLAG_M) != 0
    }

    /// True when X/Y index registers operate in 8-bit mode.
    #[inline]
    pub fn xy_is_8bit(&self) -> bool {
        self.emulation || (self.p & instructions::FLAG_X) != 0
    }

    // ---- Memory helpers ----

    /// Construct a 24-bit address from bank + offset.
    #[inline]
    pub(crate) fn bank_addr(bank: u8, offset: u16) -> u32 {
        (bank as u32) << 16 | offset as u32
    }

    #[inline]
    pub(crate) fn fetch_byte(&mut self, bus: &mut dyn Addressable) -> u8 {
        let addr = Self::bank_addr(self.pbr, self.pc);
        let val = bus.read(addr);
        self.pc = self.pc.wrapping_add(1);
        val
    }

    #[inline]
    pub(crate) fn fetch_word(&mut self, bus: &mut dyn Addressable) -> u16 {
        let lo = self.fetch_byte(bus) as u16;
        let hi = self.fetch_byte(bus) as u16;
        (hi << 8) | lo
    }

    #[inline]
    pub(crate) fn fetch_long(&mut self, bus: &mut dyn Addressable) -> u32 {
        let lo = self.fetch_byte(bus) as u32;
        let mid = self.fetch_byte(bus) as u32;
        let hi = self.fetch_byte(bus) as u32;
        (hi << 16) | (mid << 8) | lo
    }

    /// Raw single-byte push: writes at the current SP, decrements SP by
    /// one with 16-bit wrap, and does NOT re-apply the emulation-mode
    /// `$01xx` clamp.
    ///
    /// Use this when composing multi-byte stack sequences (JSL, RTL,
    /// service_interrupt, push_word, pull_word). Call `finalize_emu_sp`
    /// once at the end of the transaction to restore the canonical SP.
    #[inline]
    pub(crate) fn push_byte_raw(&mut self, bus: &mut dyn Addressable, val: u8) {
        bus.write(self.sp as u32, val);
        self.sp = self.sp.wrapping_sub(1);
    }

    /// Raw single-byte pull: increments SP first (16-bit wrap, no clamp),
    /// then reads at the new SP.
    #[inline]
    pub(crate) fn pull_byte_raw(&mut self, bus: &mut dyn Addressable) -> u8 {
        self.sp = self.sp.wrapping_add(1);
        bus.read(self.sp as u32)
    }

    /// Re-apply the emulation-mode invariant `SP & 0xFF00 == 0x0100`.
    /// No-op in native mode.
    #[inline]
    pub(crate) fn finalize_emu_sp(&mut self) {
        if self.emulation {
            self.sp = 0x0100 | (self.sp & 0xFF);
        }
    }

    /// Single-byte push for 6502-compatible instructions (PHA, PHP, PHX,
    /// PHY, PHB, PHK, JSR, RTS, RTI, BRK, COP). Writes at the current
    /// SP, then decrements with the emulation-mode `$01xx` clamp so
    /// successive calls naturally wrap inside page 1 of bank 0.
    ///
    /// For multi-byte 65816-native pushes (PHD, PEA, JSL ...) that need
    /// to span across `$0100 -> $00FF`, use [`Self::push_byte_raw`]
    /// followed by [`Self::finalize_emu_sp`] instead.
    #[inline]
    pub(crate) fn push_byte(&mut self, bus: &mut dyn Addressable, val: u8) {
        self.push_byte_raw(bus, val);
        self.finalize_emu_sp();
    }

    /// Single-byte pull for 6502-compatible instructions (PLA, PLP, PLX,
    /// PLY, PLB, RTS, RTI). Increments SP first, in emulation mode the
    /// increment is page-clamped before the read, so `$01FF + 1` reads
    /// from `$0100`, never from `$0200`.
    ///
    /// For multi-byte 65816-native pulls (PLD, RTL) that span the
    /// boundary, use [`Self::pull_byte_raw`] followed by
    /// [`Self::finalize_emu_sp`].
    #[inline]
    pub(crate) fn pull_byte(&mut self, bus: &mut dyn Addressable) -> u8 {
        self.sp = self.sp.wrapping_add(1);
        if self.emulation {
            self.sp = 0x0100 | (self.sp & 0xFF);
        }
        bus.read(self.sp as u32)
    }

    /// 16-bit push: writes both bytes contiguously (high then low) using
    /// raw decrements, then clamps SP once at the end. The intermediate
    /// SP is not forced into `$01xx`, matching the silicon behaviour where
    /// the wrap only happens after the full multi-byte transaction.
    #[inline]
    pub(crate) fn push_word(&mut self, bus: &mut dyn Addressable, val: u16) {
        self.push_byte_raw(bus, (val >> 8) as u8);
        self.push_byte_raw(bus, val as u8);
        self.finalize_emu_sp();
    }

    #[inline]
    pub(crate) fn pull_word(&mut self, bus: &mut dyn Addressable) -> u16 {
        let lo = self.pull_byte_raw(bus) as u16;
        let hi = self.pull_byte_raw(bus) as u16;
        self.finalize_emu_sp();
        (hi << 8) | lo
    }

    /// Read 16-bit little-endian from a full 24-bit address.
    #[inline]
    #[allow(dead_code)]
    pub(crate) fn read_word_bank(bus: &mut dyn Addressable, addr: u32) -> u16 {
        let lo = bus.read(addr) as u16;
        let hi = bus.read(addr.wrapping_add(1)) as u16;
        (hi << 8) | lo
    }

    /// Read 16-bit little-endian from a direct-page pointer, wrapping the
    /// high byte within bank 0 (`$00FFFF + 1` -> `$000000`, not `$010000`).
    ///
    /// Used by indirect addressing modes that derive their pointer from
    /// the direct page register: `(dp)`, `(dp,X)`, `(dp),Y`.
    #[inline]
    pub(crate) fn read_word_dp(bus: &mut dyn Addressable, ptr: u32) -> u16 {
        let lo = bus.read(ptr) as u16;
        let next = (ptr & 0xFF_0000) | (ptr.wrapping_add(1) & 0x00_FFFF);
        let hi = bus.read(next) as u16;
        (hi << 8) | lo
    }

    /// Read 24-bit little-endian, wrapping within the same bank.
    ///
    /// Used by indirect-long addressing modes whose pointer lives in bank 0
    /// (e.g. `[dp]`, `[dp],Y`, `JML [abs]`): the second and third bytes
    /// wrap at the bank boundary instead of crossing into the next bank.
    #[inline]
    pub(crate) fn read_long_dp(bus: &mut dyn Addressable, addr: u32) -> u32 {
        let lo = bus.read(addr) as u32;
        let mid = bus.read((addr & 0xFF_0000) | (addr.wrapping_add(1) & 0x00_FFFF)) as u32;
        let hi = bus.read((addr & 0xFF_0000) | (addr.wrapping_add(2) & 0x00_FFFF)) as u32;
        (hi << 16) | (mid << 8) | lo
    }

    /// Read 24-bit little-endian from a full 24-bit address.
    #[inline]
    #[allow(dead_code)]
    pub(crate) fn read_long(bus: &mut dyn Addressable, addr: u32) -> u32 {
        let lo = bus.read(addr) as u32;
        let mid = bus.read(addr.wrapping_add(1)) as u32;
        let hi = bus.read(addr.wrapping_add(2)) as u32;
        (hi << 16) | (mid << 8) | lo
    }

    // ---- Accumulator access helpers ----

    /// Get the low byte of the accumulator (A).
    #[inline]
    pub(crate) fn a_lo(&self) -> u8 {
        self.c as u8
    }

    /// Set the low byte of the accumulator, preserving B (high byte).
    #[inline]
    pub(crate) fn set_a_lo(&mut self, val: u8) {
        self.c = (self.c & 0xFF00) | val as u16;
    }
}

impl Default for Wdc65816 {
    fn default() -> Self {
        Self::new()
    }
}

// ---- Opcode dispatch ----

impl Wdc65816 {
    pub(crate) fn execute(
        &mut self,
        bus: &mut dyn Addressable,
        _opcode: u8,
        info: &decode::OpcodeInfo,
    ) -> u32 {
        use decode::Mnemonic::*;
        match info.mnemonic {
            LDA => self.lda(bus, info),
            LDX => self.ldx(bus, info),
            LDY => self.ldy(bus, info),
            STA => self.sta(bus, info),
            STX => self.stx(bus, info),
            STY => self.sty(bus, info),
            STZ => self.stz(bus, info),
            TAX => {
                self.tax();
                0
            }
            TAY => {
                self.tay();
                0
            }
            TXA => {
                self.txa();
                0
            }
            TYA => {
                self.tya();
                0
            }
            TSX => {
                self.tsx();
                0
            }
            TXS => {
                self.txs();
                0
            }
            TCD => {
                self.tcd();
                0
            }
            TCS => {
                self.tcs();
                0
            }
            TDC => {
                self.tdc();
                0
            }
            TSC => {
                self.tsc();
                0
            }
            TXY => {
                self.txy();
                0
            }
            TYX => {
                self.tyx();
                0
            }

            ADC => self.adc(bus, info),
            SBC => self.sbc(bus, info),
            INC if info.mode == addressing::AddressingMode::Accumulator => {
                self.inc_a();
                0
            }
            INC => self.inc_mem(bus, info),
            DEC if info.mode == addressing::AddressingMode::Accumulator => {
                self.dec_a();
                0
            }
            DEC => self.dec_mem(bus, info),
            INX => {
                self.inx();
                0
            }
            INY => {
                self.iny();
                0
            }
            DEX => {
                self.dex();
                0
            }
            DEY => {
                self.dey();
                0
            }

            AND => self.and(bus, info),
            ORA => self.ora(bus, info),
            EOR => self.eor(bus, info),
            BIT => self.bit(bus, info),
            TRB => self.trb(bus, info),
            TSB => self.tsb(bus, info),
            CMP => self.cmp(bus, info),
            CPX => self.cpx(bus, info),
            CPY => self.cpy(bus, info),

            ASL if info.mode == addressing::AddressingMode::Accumulator => {
                self.asl_a();
                0
            }
            ASL => self.asl_mem(bus, info),
            LSR if info.mode == addressing::AddressingMode::Accumulator => {
                self.lsr_a();
                0
            }
            LSR => self.lsr_mem(bus, info),
            ROL if info.mode == addressing::AddressingMode::Accumulator => {
                self.rol_a();
                0
            }
            ROL => self.rol_mem(bus, info),
            ROR if info.mode == addressing::AddressingMode::Accumulator => {
                self.ror_a();
                0
            }
            ROR => self.ror_mem(bus, info),

            BCC => self.bcc(bus),
            BCS => self.bcs(bus),
            BEQ => self.beq(bus),
            BMI => self.bmi(bus),
            BNE => self.bne(bus),
            BPL => self.bpl(bus),
            BVC => self.bvc(bus),
            BVS => self.bvs(bus),
            BRA => self.bra(bus),
            BRL => self.brl(bus),

            PHA => {
                self.pha(bus);
                0
            }
            PHP => {
                self.php(bus);
                0
            }
            PLA => {
                self.pla(bus);
                0
            }
            PLP => {
                self.plp(bus);
                0
            }
            PHX => {
                self.phx(bus);
                0
            }
            PHY => {
                self.phy(bus);
                0
            }
            PLX => {
                self.plx(bus);
                0
            }
            PLY => {
                self.ply(bus);
                0
            }
            PHB => {
                self.phb(bus);
                0
            }
            PLB => {
                self.plb(bus);
                0
            }
            PHD => {
                self.phd(bus);
                0
            }
            PLD => {
                self.pld(bus);
                0
            }
            PHK => {
                self.phk(bus);
                0
            }
            PEA => {
                self.pea(bus);
                0
            }
            PEI => {
                self.pei(bus);
                0
            }
            PER => {
                self.per(bus);
                0
            }

            BRK => self.brk(bus),
            COP => self.cop(bus),
            RTI => {
                self.rti(bus);
                0
            }
            RTS => {
                self.rts(bus);
                0
            }
            RTL => {
                self.rtl(bus);
                0
            }
            JSR => {
                self.jsr(bus, info);
                0
            }
            JSL => {
                self.jsl(bus);
                0
            }
            JMP => {
                self.jmp(bus, info);
                0
            }
            JML => {
                self.jml(bus, info);
                0
            }
            NOP => 0,
            WDM => {
                self.pc = self.pc.wrapping_add(1);
                0
            } // skip signature byte
            WAI => {
                self.waiting = true;
                0
            }
            STP => {
                self.stopped = true;
                0
            }
            XBA => {
                self.xba();
                0
            }
            XCE => {
                self.xce();
                0
            }

            CLC => {
                self.set_flag(instructions::FLAG_C, false);
                0
            }
            CLD => {
                self.set_flag(instructions::FLAG_D, false);
                0
            }
            CLI => {
                self.set_flag(instructions::FLAG_I, false);
                0
            }
            CLV => {
                self.set_flag(instructions::FLAG_V, false);
                0
            }
            SEC => {
                self.set_flag(instructions::FLAG_C, true);
                0
            }
            SED => {
                self.set_flag(instructions::FLAG_D, true);
                0
            }
            SEI => {
                self.set_flag(instructions::FLAG_I, true);
                0
            }
            REP => {
                self.rep(bus);
                0
            }
            SEP => {
                self.sep(bus);
                0
            }

            MVN => self.mvn(bus),
            MVP => self.mvp(bus),

            ILL => {
                let skip = info.mode.operand_size(self.a_is_8bit(), self.xy_is_8bit());
                self.pc = self.pc.wrapping_add(skip as u16);
                0
            }
        }
    }
}

impl Clockable for Wdc65816 {
    fn clock(&mut self, _cycles: u32) -> u32 {
        0
    }
}

impl Resettable for Wdc65816 {
    fn reset(&mut self) {
        self.c = 0;
        self.x = 0;
        self.y = 0;
        self.sp = 0x01FD;
        self.pc = 0;
        self.pbr = 0;
        self.dbr = 0;
        self.d = 0;
        self.p = 0x34;
        self.emulation = true;
        self.waiting = false;
        self.stopped = false;
        self.nmi_pending = false;
        self.irq_pending = false;
        self.nmi_edge_detected = false;
        self.cycles_executed = 0;
    }
}

#[cfg(test)]
mod tests;
