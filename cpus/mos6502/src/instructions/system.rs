//! System instructions: NOP, BRK, RTI, RTS, JSR, JMP.

use crate::addressing::AddressingMode;
use crate::decode::OpcodeInfo;
use crate::instructions::*;
use crate::interrupts::IRQ_VECTOR;
use crate::Mos6502;
use retro_core::cpu::Addressable;

impl Mos6502 {
    pub(crate) fn nop(&self) {
        // No operation
    }

    pub(crate) fn brk(&mut self, bus: &mut dyn Addressable) -> u32 {
        // BRK pushes PC+1 (the byte after BRK's operand padding byte)
        self.pc = self.pc.wrapping_add(1);
        self.service_interrupt(bus, IRQ_VECTOR, true);
        0
    }

    pub(crate) fn rti(&mut self, bus: &mut dyn Addressable) -> u32 {
        self.p = (self.pull_byte(bus) | FLAG_U) & !FLAG_B;
        self.pc = self.pull_word(bus);
        0
    }

    pub(crate) fn rts(&mut self, bus: &mut dyn Addressable) -> u32 {
        self.pc = self.pull_word(bus).wrapping_add(1);
        0
    }

    pub(crate) fn jsr(&mut self, bus: &mut dyn Addressable) -> u32 {
        let target = self.fetch_word(bus);
        // JSR pushes the address of the last byte of the JSR instruction (PC - 1)
        self.push_word(bus, self.pc.wrapping_sub(1));
        self.pc = target;
        0
    }

    pub(crate) fn jmp(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        if info.mode == AddressingMode::Indirect {
            let ptr = self.fetch_word(bus);
            self.pc = self.read_word_bug(bus, ptr);
        } else {
            // Absolute
            self.pc = self.fetch_word(bus);
        }
        0
    }
}
