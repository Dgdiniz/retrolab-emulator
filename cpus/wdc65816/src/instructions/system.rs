//! System control instructions: NOP, BRK, COP, RTI, RTS, RTL,
//! JSR, JSL, JMP, JML, WAI, STP, WDM, XBA, XCE.

use crate::addressing::AddressingMode;
use crate::decode::OpcodeInfo;
use crate::instructions::{FLAG_C, FLAG_M, FLAG_X};
use crate::interrupts;
use crate::Wdc65816;
use retro_core::cpu::Addressable;

impl Wdc65816 {
    /// BRK: Software interrupt.
    pub(crate) fn brk(&mut self, bus: &mut dyn Addressable) -> u32 {
        self.pc = self.pc.wrapping_add(1); // skip signature byte
        let vector = if self.emulation {
            interrupts::EMU_IRQ_VECTOR
        } else {
            interrupts::NAT_BRK_VECTOR
        };
        self.service_interrupt(bus, vector, true);
        0
    }

    /// COP: Coprocessor interrupt.
    pub(crate) fn cop(&mut self, bus: &mut dyn Addressable) -> u32 {
        self.pc = self.pc.wrapping_add(1); // skip signature byte
        let vector = if self.emulation {
            interrupts::EMU_COP_VECTOR
        } else {
            interrupts::NAT_COP_VECTOR
        };
        self.service_interrupt(bus, vector, true);
        0
    }

    /// RTI: Return from interrupt.
    ///
    /// RTI is 6502-compatible, each byte uses `pull_byte` so the
    /// emulation-mode `$01xx` clamp is re-applied after every read,
    /// keeping the stack inside page 1 of bank 0.
    pub(crate) fn rti(&mut self, bus: &mut dyn Addressable) {
        if self.emulation {
            let p = self.pull_byte(bus);
            self.p = p | FLAG_M | FLAG_X;
            let lo = self.pull_byte(bus) as u16;
            let hi = self.pull_byte(bus) as u16;
            self.pc = (hi << 8) | lo;
        } else {
            let p = self.pull_byte(bus);
            let old_x = self.p & FLAG_X;
            self.p = p;
            if (self.p & FLAG_X) != 0 && old_x == 0 {
                self.on_index_to_8bit();
            }
            let lo = self.pull_byte(bus) as u16;
            let hi = self.pull_byte(bus) as u16;
            self.pc = (hi << 8) | lo;
            self.pbr = self.pull_byte(bus);
        }
    }

    /// RTS: Return from subroutine (same bank).
    ///
    /// 6502-compatible: each byte is pulled with `pull_byte` so the
    /// stack stays inside `$01xx` in emulation mode (`$01FF + 1`
    /// wraps to `$0100`, not `$0200`).
    pub(crate) fn rts(&mut self, bus: &mut dyn Addressable) {
        let lo = self.pull_byte(bus) as u16;
        let hi = self.pull_byte(bus) as u16;
        self.pc = ((hi << 8) | lo).wrapping_add(1);
    }

    /// RTL: Return from subroutine long (pulls PBR too).
    ///
    /// RTL is a native-65816 instruction, so we keep the contiguous
    /// raw-pull pattern plus a single `finalize_emu_sp` at the end,
    /// the three bytes (PC-low / PC-high / PBR) come from consecutive
    /// addresses, crossing the `$01FF -> $0200` boundary if needed.
    pub(crate) fn rtl(&mut self, bus: &mut dyn Addressable) {
        let lo = self.pull_byte_raw(bus) as u16;
        let hi = self.pull_byte_raw(bus) as u16;
        let pbr = self.pull_byte_raw(bus);
        self.finalize_emu_sp();
        self.pc = ((hi << 8) | lo).wrapping_add(1);
        self.pbr = pbr;
    }

    /// JSR: Jump to subroutine (absolute or absolute indexed indirect).
    ///
    /// JSR is a 6502-compatible instruction, so each byte of the return
    /// address is pushed with `push_byte` (which re-clamps SP into
    /// `$01xx` after every store in emulation mode). PHA/PHX/PHD-style
    /// cross-page pushes do not apply here.
    pub(crate) fn jsr(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) {
        match info.mode {
            AddressingMode::Absolute => {
                let target = self.fetch_word(bus);
                let pc = self.pc.wrapping_sub(1);
                self.push_byte(bus, (pc >> 8) as u8);
                self.push_byte(bus, pc as u8);
                self.pc = target;
            }
            AddressingMode::AbsoluteXIndirect => {
                let base = self.fetch_word(bus);
                let pc = self.pc.wrapping_sub(1);
                self.push_byte(bus, (pc >> 8) as u8);
                self.push_byte(bus, pc as u8);
                let idx = if self.xy_is_8bit() {
                    self.x & 0xFF
                } else {
                    self.x
                };
                let ptr = Self::bank_addr(self.pbr, base.wrapping_add(idx));
                let target = Self::read_word_dp(bus, ptr);
                self.pc = target;
            }
            _ => {}
        }
    }

    /// JSL: Jump to subroutine long (24-bit).
    ///
    /// PBR / PC-high / PC-low are pushed contiguously with raw SP
    /// decrements; `finalize_emu_sp` clamps SP once at the end so the
    /// three bytes land at consecutive addresses inside bank 0 even
    /// when SP starts at `$0100`.
    pub(crate) fn jsl(&mut self, bus: &mut dyn Addressable) {
        let target = self.fetch_long(bus);
        let pc = self.pc.wrapping_sub(1);
        self.push_byte_raw(bus, self.pbr);
        self.push_byte_raw(bus, (pc >> 8) as u8);
        self.push_byte_raw(bus, pc as u8);
        self.finalize_emu_sp();
        self.pc = target as u16;
        self.pbr = (target >> 16) as u8;
    }

    /// JMP: Jump (absolute or indirect).
    pub(crate) fn jmp(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) {
        match info.mode {
            AddressingMode::Absolute => {
                self.pc = self.fetch_word(bus);
            }
            AddressingMode::AbsoluteIndirect => {
                let ptr = self.fetch_word(bus);
                self.pc = Self::read_word_dp(bus, ptr as u32);
            }
            AddressingMode::AbsoluteXIndirect => {
                let base = self.fetch_word(bus);
                let idx = if self.xy_is_8bit() {
                    self.x & 0xFF
                } else {
                    self.x
                };
                let ptr = Self::bank_addr(self.pbr, base.wrapping_add(idx));
                self.pc = Self::read_word_dp(bus, ptr);
            }
            _ => {}
        }
    }

    /// JML: Jump long (24-bit).
    pub(crate) fn jml(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) {
        match info.mode {
            AddressingMode::AbsoluteLong => {
                let target = self.fetch_long(bus);
                self.pc = target as u16;
                self.pbr = (target >> 16) as u8;
            }
            AddressingMode::AbsoluteIndirectLong => {
                let ptr = self.fetch_word(bus);
                let target = Self::read_long_dp(bus, ptr as u32);
                self.pc = target as u16;
                self.pbr = (target >> 16) as u8;
            }
            _ => {}
        }
    }

    /// XBA: Exchange B and A (swap high and low bytes of C).
    pub(crate) fn xba(&mut self) {
        self.c = self.c.rotate_left(8);
        self.set_nz_8(self.c as u8); // flags always based on new low byte (A)
    }

    /// XCE: Exchange Carry and Emulation flags.
    pub(crate) fn xce(&mut self) {
        let old_carry = self.get_flag(FLAG_C);
        let old_emulation = self.emulation;
        self.set_flag(FLAG_C, old_emulation);
        self.emulation = old_carry;

        if self.emulation {
            // Entering emulation mode: force M=1, X=1, SP to page $01
            self.p |= FLAG_M | FLAG_X;
            self.on_index_to_8bit();
            self.sp = 0x0100 | (self.sp & 0xFF);
        }
    }
}
