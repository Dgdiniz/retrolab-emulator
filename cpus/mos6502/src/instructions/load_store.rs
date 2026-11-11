//! Load, store, and register transfer instructions.

use crate::addressing;
use crate::decode::OpcodeInfo;
use crate::Mos6502;
use retro_core::cpu::Addressable;

impl Mos6502 {
    // ---- Loads ----

    pub(crate) fn lda(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let res = addressing::resolve_address(bus, info.mode, self.pc, self.x, self.y);
        self.pc = self.pc.wrapping_add(info.mode.operand_size() as u16);
        self.a = bus.read(res.addr as u32);
        self.set_nz(self.a);
        if info.page_penalty && res.page_crossed {
            1
        } else {
            0
        }
    }

    pub(crate) fn ldx(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let res = addressing::resolve_address(bus, info.mode, self.pc, self.x, self.y);
        self.pc = self.pc.wrapping_add(info.mode.operand_size() as u16);
        self.x = bus.read(res.addr as u32);
        self.set_nz(self.x);
        if info.page_penalty && res.page_crossed {
            1
        } else {
            0
        }
    }

    pub(crate) fn ldy(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let res = addressing::resolve_address(bus, info.mode, self.pc, self.x, self.y);
        self.pc = self.pc.wrapping_add(info.mode.operand_size() as u16);
        self.y = bus.read(res.addr as u32);
        self.set_nz(self.y);
        if info.page_penalty && res.page_crossed {
            1
        } else {
            0
        }
    }

    // ---- Stores ----

    pub(crate) fn sta(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let res = addressing::resolve_address(bus, info.mode, self.pc, self.x, self.y);
        self.pc = self.pc.wrapping_add(info.mode.operand_size() as u16);
        bus.write(res.addr as u32, self.a);
        0
    }

    pub(crate) fn stx(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let res = addressing::resolve_address(bus, info.mode, self.pc, self.x, self.y);
        self.pc = self.pc.wrapping_add(info.mode.operand_size() as u16);
        bus.write(res.addr as u32, self.x);
        0
    }

    pub(crate) fn sty(&mut self, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
        let res = addressing::resolve_address(bus, info.mode, self.pc, self.x, self.y);
        self.pc = self.pc.wrapping_add(info.mode.operand_size() as u16);
        bus.write(res.addr as u32, self.y);
        0
    }

    // ---- Register transfers ----

    pub(crate) fn tax(&mut self) {
        self.x = self.a;
        self.set_nz(self.x);
    }

    pub(crate) fn tay(&mut self) {
        self.y = self.a;
        self.set_nz(self.y);
    }

    pub(crate) fn txa(&mut self) {
        self.a = self.x;
        self.set_nz(self.a);
    }

    pub(crate) fn tya(&mut self) {
        self.a = self.y;
        self.set_nz(self.a);
    }

    pub(crate) fn tsx(&mut self) {
        self.x = self.sp;
        self.set_nz(self.x);
    }

    pub(crate) fn txs(&mut self) {
        self.sp = self.x;
        // TXS does NOT affect flags
    }
}
