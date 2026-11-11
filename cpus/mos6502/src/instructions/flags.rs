//! Flag manipulation instructions: CLC, CLD, CLI, CLV, SEC, SED, SEI.

use crate::instructions::*;
use crate::Mos6502;

impl Mos6502 {
    pub(crate) fn clc(&mut self) {
        self.set_flag(FLAG_C, false);
    }

    pub(crate) fn cld(&mut self) {
        self.set_flag(FLAG_D, false);
    }

    pub(crate) fn cli(&mut self) {
        self.set_flag(FLAG_I, false);
    }

    pub(crate) fn clv(&mut self) {
        self.set_flag(FLAG_V, false);
    }

    pub(crate) fn sec(&mut self) {
        self.set_flag(FLAG_C, true);
    }

    pub(crate) fn sed(&mut self) {
        self.set_flag(FLAG_D, true);
    }

    pub(crate) fn sei(&mut self) {
        self.set_flag(FLAG_I, true);
    }
}
