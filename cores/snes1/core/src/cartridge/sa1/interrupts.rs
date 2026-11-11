//! The SA-1's interrupts, both ways.
//!
//! **To the SA-1.** The S-CPU sends an IRQ or an NMI, with a four-bit
//! message, through CCNT (`$2200`); the SA-1's timer and the end of a copy
//! raise IRQs of their own. The SA-1 takes an NMI from the vector in CNV
//! (`$2205-$2206`) and any IRQ from CIV (`$2207-$2208`), not from the
//! vectors at the top of bank `$00`. Its flags read at CFR (`$2301`), its
//! enables are CIE (`$220A`), its acknowledgements CIC (`$220B`).
//!
//! **To the S-CPU.** The SA-1 sends an IRQ, with a message, through SCNT
//! (`$2209`), and arming a type-1 character conversion raises one too. The
//! flags read at SFR (`$2300`), the enables are SIE (`$2201`), the
//! acknowledgements SIC (`$2202`). SCNT can also make the S-CPU's own NMI and
//! IRQ vectors read from SNV (`$220C-$220D`) and SIV (`$220E-$220F`).
//!
//! Each source has three latches: the flag the receiving CPU reads, the
//! enable, and an acknowledgement. When a source fires its flag goes up and,
//! if it is enabled, its acknowledgement goes down; writing 1 to its bit in
//! the acknowledgement register drops the flag. Enabling a source whose flag
//! is up makes it assert again. On the SA-1's side the timer and the copy
//! assert while they are enabled and not acknowledged, and the S-CPU's
//! messages while the CCNT bit that sent them is set and they are not
//! acknowledged. The line to the S-CPU is held until an acknowledgement
//! leaves neither of its flags up.
//!
//! Behaviour checked against bsnes-plus and ares, which agree.

use super::registers::{field, flag, pack, Bytes};
use crate::savestate::Sa1IrqState;

/// The three latches of one interrupt source.
#[derive(Clone, Copy, Default)]
struct Source {
    /// Fired and not yet acknowledged: the bit the receiving CPU reads.
    flag: bool,
    /// The program's enable.
    enabled: bool,
    /// Acknowledged: an enabled source does not assert while this is set.
    acknowledged: bool,
}

impl Source {
    /// The source fires. Returns whether it is enabled (and so asserts).
    fn fire(&mut self) -> bool {
        self.flag = true;
        if self.enabled {
            self.acknowledged = false;
        }
        self.enabled
    }

    /// A write of the source's enable bit. Enabling it while its flag is up
    /// makes it assert again; returns whether it did.
    fn write_enable(&mut self, on: bool) -> bool {
        let reasserts = !self.enabled && on && self.flag;
        if reasserts {
            self.acknowledged = false;
        }
        self.enabled = on;
        reasserts
    }

    /// A write of the source's acknowledgement bit; acknowledging drops the
    /// flag.
    fn write_acknowledge(&mut self, on: bool) {
        self.acknowledged = on;
        if on {
            self.flag = false;
        }
    }

    /// Whether an enabled source asserts.
    fn asserts(&self) -> bool {
        self.enabled && !self.acknowledged
    }
}

/// Both directions of the SA-1's interrupts.
#[derive(Default)]
pub(super) struct Interrupts {
    // To the SA-1, CFR bits 7 to 4, CIE and CIC the same bits.
    /// An IRQ the S-CPU sent through CCNT.
    from_scpu_irq: Source,
    /// The H/V timer reached its target.
    timer: Source,
    /// A copy finished.
    copy_done: Source,
    /// An NMI the S-CPU sent through CCNT.
    from_scpu_nmi: Source,
    /// CCNT (`$2200`) bit 7: the S-CPU's IRQ is being sent.
    irq_sent: bool,
    /// CCNT bit 4: the S-CPU's NMI is being sent.
    nmi_sent: bool,
    /// CCNT bits 0-3: the S-CPU's message, read at CFR bits 0-3.
    message_from_scpu: u8,
    /// CNV (`$2205-$2206`): where the SA-1 takes an NMI.
    nmi_vector: u16,
    /// CIV (`$2207-$2208`): where the SA-1 takes an IRQ.
    irq_vector: u16,
    // To the S-CPU, SFR bits 7 and 5, SIE and SIC the same bits.
    /// An IRQ the SA-1 sent through SCNT.
    from_sa1: Source,
    /// A type-1 character conversion was armed.
    conversion_armed: Source,
    /// SCNT (`$2209`) bit 6: the S-CPU's IRQ vector reads from SIV.
    irq_vector_from_sa1: bool,
    /// SCNT bit 4: the S-CPU's NMI vector reads from SNV.
    nmi_vector_from_sa1: bool,
    /// SCNT bits 0-3: the SA-1's message, read at SFR bits 0-3.
    message_to_scpu: u8,
    /// SNV (`$220C-$220D`): the S-CPU's NMI vector, when SCNT says so.
    scpu_nmi_vector: u16,
    /// SIV (`$220E-$220F`): the S-CPU's IRQ vector, when SCNT says so.
    scpu_irq_vector: u16,
    /// The IRQ line to the S-CPU.
    scpu_line: bool,
    /// The SA-1's NMI line as the CPU last saw it, so a line held up is
    /// taken once.
    nmi_line_seen: bool,
}

impl Interrupts {
    /// The interrupt part of CCNT (`$2200`): bits 7 and 4 send an IRQ and an
    /// NMI to the SA-1, bits 0-3 a message.
    pub(super) fn write_scpu_control(&mut self, value: u8) {
        self.irq_sent = flag(value, 7);
        self.nmi_sent = flag(value, 4);
        self.message_from_scpu = field(value, 0, 4);
        if self.irq_sent {
            self.from_scpu_irq.fire();
        }
        if self.nmi_sent {
            self.from_scpu_nmi.fire();
        }
    }

    /// SIE (`$2201`): the enables of the S-CPU's two sources.
    pub(super) fn write_scpu_enable(&mut self, value: u8) {
        let message = self.from_sa1.write_enable(flag(value, 7));
        let conversion = self.conversion_armed.write_enable(flag(value, 5));
        if message || conversion {
            self.scpu_line = true;
        }
    }

    /// SIC (`$2202`): the S-CPU acknowledges its sources; its line drops
    /// once neither flag is up.
    pub(super) fn write_scpu_acknowledge(&mut self, value: u8) {
        self.from_sa1.write_acknowledge(flag(value, 7));
        self.conversion_armed.write_acknowledge(flag(value, 5));
        if !self.from_sa1.flag && !self.conversion_armed.flag {
            self.scpu_line = false;
        }
    }

    /// SCNT (`$2209`): bit 7 sends an IRQ to the S-CPU, bits 6 and 4 choose
    /// where the S-CPU's vectors read from, bits 0-3 are the message.
    pub(super) fn write_sa1_control(&mut self, value: u8) {
        self.irq_vector_from_sa1 = flag(value, 6);
        self.nmi_vector_from_sa1 = flag(value, 4);
        self.message_to_scpu = field(value, 0, 4);
        if flag(value, 7) && self.from_sa1.fire() {
            self.scpu_line = true;
        }
    }

    /// CIE (`$220A`): the enables of the SA-1's four sources.
    pub(super) fn write_sa1_enable(&mut self, value: u8) {
        self.from_scpu_irq.write_enable(flag(value, 7));
        self.timer.write_enable(flag(value, 6));
        self.copy_done.write_enable(flag(value, 5));
        self.from_scpu_nmi.write_enable(flag(value, 4));
    }

    /// CIC (`$220B`): the SA-1 acknowledges its sources.
    pub(super) fn write_sa1_acknowledge(&mut self, value: u8) {
        self.from_scpu_irq.write_acknowledge(flag(value, 7));
        self.timer.write_acknowledge(flag(value, 6));
        self.copy_done.write_acknowledge(flag(value, 5));
        self.from_scpu_nmi.write_acknowledge(flag(value, 4));
    }

    /// One byte of CNV (`$2205` + `index`).
    pub(super) fn write_nmi_vector(&mut self, index: usize, value: u8) {
        self.nmi_vector = self.nmi_vector.with_byte(index, value);
    }

    /// One byte of CIV (`$2207` + `index`).
    pub(super) fn write_irq_vector(&mut self, index: usize, value: u8) {
        self.irq_vector = self.irq_vector.with_byte(index, value);
    }

    /// One byte of SNV (`$220C` + `index`).
    pub(super) fn write_scpu_nmi_vector(&mut self, index: usize, value: u8) {
        self.scpu_nmi_vector = self.scpu_nmi_vector.with_byte(index, value);
    }

    /// One byte of SIV (`$220E` + `index`).
    pub(super) fn write_scpu_irq_vector(&mut self, index: usize, value: u8) {
        self.scpu_irq_vector = self.scpu_irq_vector.with_byte(index, value);
    }

    /// The timer reached its target.
    pub(super) fn timer_fired(&mut self) {
        self.timer.fire();
    }

    /// A copy finished.
    pub(super) fn copy_finished(&mut self) {
        self.copy_done.fire();
    }

    /// A type-1 character conversion was armed: an IRQ to the S-CPU.
    pub(super) fn conversion_armed(&mut self) {
        if self.conversion_armed.fire() {
            self.scpu_line = true;
        }
    }

    /// SFR (`$2300`), as the S-CPU reads it.
    pub(super) fn scpu_flags(&self) -> u8 {
        let bits = [
            (self.from_sa1.flag, 7),
            (self.irq_vector_from_sa1, 6),
            (self.conversion_armed.flag, 5),
            (self.nmi_vector_from_sa1, 4),
        ];
        pack(bits, self.message_to_scpu)
    }

    /// CFR (`$2301`), as the SA-1 reads it.
    pub(super) fn sa1_flags(&self) -> u8 {
        let bits = [
            (self.from_scpu_irq.flag, 7),
            (self.timer.flag, 6),
            (self.copy_done.flag, 5),
            (self.from_scpu_nmi.flag, 4),
        ];
        pack(bits, self.message_from_scpu)
    }

    /// The SA-1's IRQ line: the timer or the copy while enabled and not
    /// acknowledged, or the S-CPU's IRQ while sent and not acknowledged.
    /// Whether the CPU takes it (its I flag) is the CPU's affair.
    pub(super) fn sa1_irq_line(&self) -> bool {
        self.timer.asserts()
            || self.copy_done.asserts()
            || (self.irq_sent && !self.from_scpu_irq.acknowledged)
    }

    /// Whether the SA-1's NMI line has just risen, the S-CPU's NMI sent and
    /// not acknowledged, where it was not when last asked. An NMI is taken on
    /// the rise, once.
    pub(super) fn nmi_rose(&mut self) -> bool {
        let line = self.nmi_sent && !self.from_scpu_nmi.acknowledged;
        let rose = line && !self.nmi_line_seen;
        self.nmi_line_seen = line;
        rose
    }

    /// The SA-1 took its NMI: the flag goes up and the NMI is acknowledged,
    /// so the line stays down until the S-CPU sends the next one.
    pub(super) fn nmi_taken(&mut self) {
        self.from_scpu_nmi.flag = true;
        self.from_scpu_nmi.acknowledged = true;
    }

    /// The IRQ line to the S-CPU.
    pub(super) fn scpu_line(&self) -> bool {
        self.scpu_line
    }

    /// The byte a vector fetch of the SA-1 reads while it takes an
    /// interrupt: CNV for an NMI and CIV for an IRQ, in place of the vectors
    /// at the top of bank `$00`, native and emulation pairs alike. `None` at
    /// any other address.
    pub(super) fn sa1_vector_byte(&self, address: u32) -> Option<u8> {
        let vector = match address & 0xff_fffe {
            0x00_ffea | 0x00_fffa => self.nmi_vector,
            0x00_ffee | 0x00_fffe => self.irq_vector,
            _ => return None,
        };
        Some(vector.byte(address as usize & 1))
    }

    /// The byte the S-CPU reads at its native NMI or IRQ vector when SCNT
    /// gives it SNV or SIV instead of ROM; `None` when it reads ROM.
    pub(super) fn scpu_vector_byte(&self, address: u32) -> Option<u8> {
        let vector = match address & 0xff_fffe {
            0x00_ffea if self.nmi_vector_from_sa1 => self.scpu_nmi_vector,
            0x00_ffee if self.irq_vector_from_sa1 => self.scpu_irq_vector,
            _ => return None,
        };
        Some(vector.byte(address as usize & 1))
    }

    // -- For the debugger -------------------------------------------------

    /// CNV, CIV, SNV and SIV.
    pub(super) fn vectors(&self) -> [u16; 4] {
        [
            self.nmi_vector,
            self.irq_vector,
            self.scpu_nmi_vector,
            self.scpu_irq_vector,
        ]
    }

    /// CIE as written (bits 7 to 4).
    pub(super) fn sa1_enable_byte(&self) -> u8 {
        let bits = [
            (self.from_scpu_irq.enabled, 7),
            (self.timer.enabled, 6),
            (self.copy_done.enabled, 5),
            (self.from_scpu_nmi.enabled, 4),
        ];
        pack(bits, 0)
    }

    /// SIE as written (bits 7 and 5).
    pub(super) fn scpu_enable_byte(&self) -> u8 {
        pack(
            [
                (self.from_sa1.enabled, 7),
                (self.conversion_armed.enabled, 5),
            ],
            0,
        )
    }

    /// The interrupts for a savestate.
    pub(super) fn snapshot(&self) -> Sa1IrqState {
        Sa1IrqState {
            sa1_irqfl: self.from_scpu_irq.flag,
            timer_irqfl: self.timer.flag,
            dma_irqfl: self.copy_done.flag,
            sa1_nmifl: self.from_scpu_nmi.flag,
            sa1_irqen: self.from_scpu_irq.enabled,
            timer_irqen: self.timer.enabled,
            dma_irqen: self.copy_done.enabled,
            sa1_nmien: self.from_scpu_nmi.enabled,
            sa1_irqcl: self.from_scpu_irq.acknowledged,
            timer_irqcl: self.timer.acknowledged,
            dma_irqcl: self.copy_done.acknowledged,
            sa1_nmicl: self.from_scpu_nmi.acknowledged,
            sa1_irq: self.irq_sent,
            sa1_nmi: self.nmi_sent,
            smeg: self.message_from_scpu,
            cnv: self.nmi_vector,
            civ: self.irq_vector,
            cpu_irqfl: self.from_sa1.flag,
            chdma_irqfl: self.conversion_armed.flag,
            cpu_irqen: self.from_sa1.enabled,
            chdma_irqen: self.conversion_armed.enabled,
            cpu_irqcl: self.from_sa1.acknowledged,
            chdma_irqcl: self.conversion_armed.acknowledged,
            cpu_ivsw: self.irq_vector_from_sa1,
            cpu_nvsw: self.nmi_vector_from_sa1,
            cmeg: self.message_to_scpu,
            snv: self.scpu_nmi_vector,
            siv: self.scpu_irq_vector,
            scpu_irq: self.scpu_line,
            prev_nmi_line: self.nmi_line_seen,
        }
    }

    /// The interrupts from a savestate.
    pub(super) fn restore(&mut self, s: &Sa1IrqState) {
        let source = |flag, enabled, acknowledged| Source {
            flag,
            enabled,
            acknowledged,
        };
        self.from_scpu_irq = source(s.sa1_irqfl, s.sa1_irqen, s.sa1_irqcl);
        self.timer = source(s.timer_irqfl, s.timer_irqen, s.timer_irqcl);
        self.copy_done = source(s.dma_irqfl, s.dma_irqen, s.dma_irqcl);
        self.from_scpu_nmi = source(s.sa1_nmifl, s.sa1_nmien, s.sa1_nmicl);
        self.irq_sent = s.sa1_irq;
        self.nmi_sent = s.sa1_nmi;
        self.message_from_scpu = s.smeg;
        self.nmi_vector = s.cnv;
        self.irq_vector = s.civ;
        self.from_sa1 = source(s.cpu_irqfl, s.cpu_irqen, s.cpu_irqcl);
        self.conversion_armed = source(s.chdma_irqfl, s.chdma_irqen, s.chdma_irqcl);
        self.irq_vector_from_sa1 = s.cpu_ivsw;
        self.nmi_vector_from_sa1 = s.cpu_nvsw;
        self.message_to_scpu = s.cmeg;
        self.scpu_nmi_vector = s.snv;
        self.scpu_irq_vector = s.siv;
        self.scpu_line = s.scpu_irq;
        self.nmi_line_seen = s.prev_nmi_line;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_from_the_sa1_reaches_the_s_cpu_only_when_enabled() {
        let mut i = Interrupts::default();
        i.write_sa1_control(0x85); // IRQ, message 5
        assert!(!i.scpu_line(), "not enabled");
        assert_eq!(i.scpu_flags(), 0x85);
        i.write_scpu_enable(0x80); // enabling with the flag up asserts
        assert!(i.scpu_line());
        i.write_scpu_acknowledge(0x80);
        assert!(!i.scpu_line());
        assert_eq!(i.scpu_flags() & 0x80, 0);
    }

    #[test]
    fn the_s_cpu_line_holds_while_either_flag_is_up() {
        let mut i = Interrupts::default();
        i.write_scpu_enable(0xa0);
        i.write_sa1_control(0x80);
        i.conversion_armed();
        i.write_scpu_acknowledge(0x80);
        assert!(i.scpu_line(), "the conversion's flag is still up");
        i.write_scpu_acknowledge(0x20);
        assert!(!i.scpu_line());
    }

    #[test]
    fn the_timer_asserts_while_enabled_and_not_acknowledged() {
        let mut i = Interrupts::default();
        i.write_sa1_acknowledge(0x40);
        i.write_sa1_enable(0x40);
        assert!(!i.sa1_irq_line(), "acknowledged");
        i.timer_fired();
        assert!(i.sa1_irq_line());
        assert_eq!(i.sa1_flags() & 0x40, 0x40);
        i.write_sa1_acknowledge(0x40);
        assert!(!i.sa1_irq_line());
        assert_eq!(i.sa1_flags() & 0x40, 0);
    }

    #[test]
    fn an_nmi_is_taken_once_on_its_rise() {
        let mut i = Interrupts::default();
        i.write_sa1_enable(0x10);
        i.write_scpu_control(0x13); // NMI, message 3
        assert_eq!(i.sa1_flags(), 0x13);
        assert!(i.nmi_rose());
        assert!(!i.nmi_rose(), "a line held up rises once");
        i.nmi_taken();
        assert!(!i.nmi_rose());
    }

    #[test]
    fn vector_fetches_read_the_registers() {
        let mut i = Interrupts::default();
        i.write_nmi_vector(0, 0x34);
        i.write_nmi_vector(1, 0x12);
        i.write_irq_vector(0, 0x78);
        i.write_irq_vector(1, 0x56);
        assert_eq!(i.sa1_vector_byte(0x00_ffea), Some(0x34));
        assert_eq!(i.sa1_vector_byte(0x00_fffb), Some(0x12));
        assert_eq!(i.sa1_vector_byte(0x00_ffef), Some(0x56));
        assert_eq!(i.sa1_vector_byte(0x00_fffe), Some(0x78));
        assert_eq!(i.sa1_vector_byte(0x00_ffec), None);
        assert_eq!(i.sa1_vector_byte(0x01_ffea), None);

        i.write_scpu_nmi_vector(1, 0xab);
        assert_eq!(
            i.scpu_vector_byte(0x00_ffeb),
            None,
            "SCNT does not ask for it"
        );
        i.write_sa1_control(0x10);
        assert_eq!(i.scpu_vector_byte(0x00_ffeb), Some(0xab));
        assert_eq!(
            i.scpu_vector_byte(0x00_fffb),
            None,
            "the emulation vector reads ROM"
        );
    }

    #[test]
    fn the_interrupts_survive_a_savestate() {
        let mut i = Interrupts::default();
        i.write_sa1_enable(0xf0);
        i.write_scpu_control(0x9a);
        i.write_sa1_control(0xd5);
        i.write_scpu_enable(0x20);
        i.conversion_armed();
        i.timer_fired();
        let snap = i.snapshot();
        let mut restored = Interrupts::default();
        restored.restore(&snap);
        assert_eq!(restored.snapshot(), snap);
    }
}
