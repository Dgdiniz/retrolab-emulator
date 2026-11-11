//! MOS 6502 CPU emulation.
//!
//! Implements the MOS Technology 6502 processor, the base CPU for the
//! Ricoh 2A03 (NES), Commodore 64, Atari 2600, Apple II, and others.
//!
//! The 2A03 variant disables BCD arithmetic mode; use `Mos6502::new_nes()`
//! for NES emulation.

pub mod addressing;
pub mod decode;
pub mod disasm;
pub mod instructions;
pub mod interrupts;

use decode::OPCODE_TABLE;
use retro_core::cpu::{Addressable, Clockable, Resettable};

/// MOS 6502 CPU.
pub struct Mos6502 {
    // Registers
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub sp: u8,
    pub pc: u16,
    /// Status register: NV-BDIZC (bits 7..0)
    pub p: u8,

    pub cycles_executed: u64,

    // Interrupt state
    pub nmi_pending: bool,
    pub irq_pending: bool,
    /// NMI is edge-triggered: track whether we already saw the falling edge
    pub nmi_edge_detected: bool,

    /// CPU jammed by illegal KIL/JAM opcode
    pub halted: bool,

    /// When true, ADC/SBC skip BCD logic (Ricoh 2A03 behaviour)
    pub bcd_disabled: bool,
}

impl Mos6502 {
    pub fn new() -> Self {
        Self {
            a: 0,
            x: 0,
            y: 0,
            sp: 0xFD,
            pc: 0,
            p: 0x24, // IRQ disabled, unused bit set
            cycles_executed: 0,
            nmi_pending: false,
            irq_pending: false,
            nmi_edge_detected: false,
            halted: false,
            bcd_disabled: false,
        }
    }

    /// Create a 6502 configured as Ricoh 2A03 (BCD disabled).
    pub fn new_nes() -> Self {
        let mut cpu = Self::new();
        cpu.bcd_disabled = true;
        cpu
    }

    /// Hardware reset: reads the reset vector and initialises state.
    pub fn reset_with_bus(&mut self, bus: &mut dyn Addressable) {
        self.a = 0;
        self.x = 0;
        self.y = 0;
        self.sp = 0xFD;
        self.p = 0x24;
        self.halted = false;
        self.nmi_pending = false;
        self.irq_pending = false;
        self.nmi_edge_detected = false;
        self.cycles_executed = 0;

        let lo = bus.read(interrupts::RESET_VECTOR as u32) as u16;
        let hi = bus.read((interrupts::RESET_VECTOR + 1) as u32) as u16;
        self.pc = (hi << 8) | lo;
    }

    /// Execute one instruction. Returns the number of cycles consumed.
    pub fn step(&mut self, bus: &mut dyn Addressable) -> u32 {
        if self.halted {
            return 1;
        }

        // Check interrupts before fetching
        if let Some(cycles) = self.poll_interrupts(bus) {
            return cycles;
        }

        let opcode = self.fetch_byte(bus);
        let info = &OPCODE_TABLE[opcode as usize];

        let extra_cycles = self.execute(bus, opcode, info);
        let total = info.cycles as u32 + extra_cycles;

        self.cycles_executed += total as u64;
        total
    }

    // ---- internal helpers ----

    #[inline]
    pub(crate) fn fetch_byte(&mut self, bus: &mut dyn Addressable) -> u8 {
        let val = bus.read(self.pc as u32);
        self.pc = self.pc.wrapping_add(1);
        val
    }

    #[inline]
    pub(crate) fn fetch_word(&mut self, bus: &mut dyn Addressable) -> u16 {
        let lo = bus.read(self.pc as u32) as u16;
        let hi = bus.read(self.pc.wrapping_add(1) as u32) as u16;
        self.pc = self.pc.wrapping_add(2);
        (hi << 8) | lo
    }

    #[inline]
    pub(crate) fn push_byte(&mut self, bus: &mut dyn Addressable, val: u8) {
        bus.write(0x0100 | self.sp as u32, val);
        self.sp = self.sp.wrapping_sub(1);
    }

    #[inline]
    pub(crate) fn pull_byte(&mut self, bus: &mut dyn Addressable) -> u8 {
        self.sp = self.sp.wrapping_add(1);
        bus.read(0x0100 | self.sp as u32)
    }

    #[inline]
    pub(crate) fn push_word(&mut self, bus: &mut dyn Addressable, val: u16) {
        self.push_byte(bus, (val >> 8) as u8);
        self.push_byte(bus, val as u8);
    }

    #[inline]
    pub(crate) fn pull_word(&mut self, bus: &mut dyn Addressable) -> u16 {
        let lo = self.pull_byte(bus) as u16;
        let hi = self.pull_byte(bus) as u16;
        (hi << 8) | lo
    }

    /// Read a 16-bit value with the 6502 page-boundary bug (used by JMP indirect).
    /// If the low byte of the pointer is 0xFF, the high byte wraps within the page
    /// instead of crossing to the next page.
    #[inline]
    pub(crate) fn read_word_bug(&self, bus: &mut dyn Addressable, addr: u16) -> u16 {
        let lo = bus.read(addr as u32) as u16;
        let hi_addr = (addr & 0xFF00) | ((addr.wrapping_add(1)) & 0x00FF);
        let hi = bus.read(hi_addr as u32) as u16;
        (hi << 8) | lo
    }

    /// Read a 16-bit little-endian value from an address (no bug).
    #[inline]
    #[allow(dead_code)]
    pub(crate) fn read_word(&self, bus: &mut dyn Addressable, addr: u16) -> u16 {
        let lo = bus.read(addr as u32) as u16;
        let hi = bus.read(addr.wrapping_add(1) as u32) as u16;
        (hi << 8) | lo
    }
}

impl Default for Mos6502 {
    fn default() -> Self {
        Self::new()
    }
}

// ---- Opcode dispatch ----

impl Mos6502 {
    /// Dispatch an opcode to the appropriate instruction handler.
    /// PC already points past the opcode byte. Each handler advances PC
    /// past its operand(s). Returns extra cycles beyond the base count.
    pub(crate) fn execute(
        &mut self,
        bus: &mut dyn Addressable,
        _opcode: u8,
        info: &decode::OpcodeInfo,
    ) -> u32 {
        use decode::Mnemonic::*;

        match info.mnemonic {
            // Load / Store / Transfer
            LDA => self.lda(bus, info),
            LDX => self.ldx(bus, info),
            LDY => self.ldy(bus, info),
            STA => self.sta(bus, info),
            STX => self.stx(bus, info),
            STY => self.sty(bus, info),
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

            // Arithmetic
            ADC => self.adc(bus, info),
            SBC => self.sbc(bus, info),
            INC => self.inc(bus, info),
            DEC => self.dec(bus, info),
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

            // Logical
            AND => self.and(bus, info),
            ORA => self.ora(bus, info),
            EOR => self.eor(bus, info),

            // Compare / Bit test
            CMP => self.cmp(bus, info),
            CPX => self.cpx(bus, info),
            CPY => self.cpy(bus, info),
            BIT => self.bit(bus, info),

            // Shifts / Rotates
            ASL => self.asl(bus, info),
            LSR => self.lsr(bus, info),
            ROL => self.rol(bus, info),
            ROR => self.ror(bus, info),

            // Branches
            BCC => self.bcc(bus),
            BCS => self.bcs(bus),
            BEQ => self.beq(bus),
            BMI => self.bmi(bus),
            BNE => self.bne(bus),
            BPL => self.bpl(bus),
            BVC => self.bvc(bus),
            BVS => self.bvs(bus),

            // Stack
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

            // System
            BRK => self.brk(bus),
            RTI => self.rti(bus),
            RTS => self.rts(bus),
            JSR => self.jsr(bus),
            JMP => self.jmp(bus, info),
            NOP => {
                self.nop();
                0
            }

            // Flags
            CLC => {
                self.clc();
                0
            }
            CLD => {
                self.cld();
                0
            }
            CLI => {
                self.cli();
                0
            }
            CLV => {
                self.clv();
                0
            }
            SEC => {
                self.sec();
                0
            }
            SED => {
                self.sed();
                0
            }
            SEI => {
                self.sei();
                0
            }

            // Illegal opcodes: skip operand bytes, act as NOP
            ILL => {
                let skip = info.mode.operand_size();
                self.pc = self.pc.wrapping_add(skip as u16);
                0
            }
        }
    }
}

impl Clockable for Mos6502 {
    fn clock(&mut self, _cycles: u32) -> u32 {
        0
    }
}

impl Resettable for Mos6502 {
    fn reset(&mut self) {
        self.a = 0;
        self.x = 0;
        self.y = 0;
        self.sp = 0xFD;
        self.p = 0x24;
        self.halted = false;
        self.nmi_pending = false;
        self.irq_pending = false;
        self.nmi_edge_detected = false;
        self.cycles_executed = 0;
        self.pc = 0;
    }
}

#[cfg(test)]
mod tests;
