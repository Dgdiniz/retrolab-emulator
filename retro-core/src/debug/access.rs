//! The memory accesses one instruction made, for the watchpoint check.
//!
//! A core records the bytes its CPU read and wrote while an instruction ran
//! (a bus with watchpoints armed logs them) and hands the log to
//! [`crate::debug::BreakpointList::check_accesses`] once the instruction is
//! done. Each access is one byte: a core whose bus moves words logs a word
//! as its two bytes ([`AccessLog::push_word_big_endian`]), so a watch on
//! either byte sees it.

/// One byte the CPU read or wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Access {
    /// The address, in the CPU's own address space.
    pub address: u32,
    /// A write; a read otherwise.
    pub write: bool,
    /// The byte read or written.
    pub value: u8,
}

/// The accesses one instruction made, in the order it made them, at most
/// [`Self::CAPACITY`]: past it an access is not kept but counted
/// ([`Self::dropped`]), as [`crate::debug::RaisedEvents`] keeps its
/// events. Kept across instructions and cleared between them, so recording
/// allocates nothing once the log has grown to the largest instruction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccessLog {
    accesses: Vec<Access>,
    dropped: u32,
}

impl AccessLog {
    /// The most accesses the log keeps: far more than any one instruction
    /// of these CPUs makes, so only a log nothing clears between
    /// instructions reaches it.
    pub const CAPACITY: usize = 4096;

    /// An empty log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget every access and the count of those dropped, keeping the
    /// allocation.
    pub fn clear(&mut self) {
        self.accesses.clear();
        self.dropped = 0;
    }

    /// Record one byte; past the capacity it is counted instead.
    #[inline]
    pub fn push(&mut self, access: Access) {
        if self.accesses.len() >= Self::CAPACITY {
            self.dropped = self.dropped.saturating_add(1);
            return;
        }
        self.accesses.push(access);
    }

    /// How many accesses were not kept for want of room since the log was
    /// made or last cleared.
    pub fn dropped(&self) -> u32 {
        self.dropped
    }

    /// Record a 16-bit access as its two bytes, most significant first: the
    /// high byte at `address`, the low byte at `address + 1` (the 68000's
    /// order).
    pub fn push_word_big_endian(&mut self, address: u32, write: bool, value: u16) {
        let [high, low] = value.to_be_bytes();
        self.push(Access {
            address,
            write,
            value: high,
        });
        self.push(Access {
            address: address.wrapping_add(1),
            write,
            value: low,
        });
    }

    /// The accesses, in the order they were made.
    pub fn as_slice(&self) -> &[Access] {
        &self.accesses
    }

    /// Whether nothing was recorded.
    pub fn is_empty(&self) -> bool {
        self.accesses.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_keeps_its_capacity_and_counts_the_rest() {
        let mut log = AccessLog::new();
        for n in 0..AccessLog::CAPACITY + 3 {
            log.push(Access {
                address: n as u32,
                write: false,
                value: 0,
            });
        }
        assert_eq!(log.as_slice().len(), AccessLog::CAPACITY);
        assert_eq!(log.as_slice().last().unwrap().address, 4095);
        assert_eq!(log.dropped(), 3);
        log.clear();
        assert!(log.is_empty());
        assert_eq!(log.dropped(), 0);
    }

    #[test]
    fn a_word_is_its_two_bytes_most_significant_first() {
        let mut log = AccessLog::new();
        log.push_word_big_endian(0xFF_0010, true, 0x1234);
        log.push(Access {
            address: 0x20,
            write: false,
            value: 0xAB,
        });
        assert_eq!(
            log.as_slice(),
            [
                Access {
                    address: 0xFF_0010,
                    write: true,
                    value: 0x12
                },
                Access {
                    address: 0xFF_0011,
                    write: true,
                    value: 0x34
                },
                Access {
                    address: 0x20,
                    write: false,
                    value: 0xAB
                },
            ]
        );
    }

    #[test]
    fn a_word_at_the_top_of_the_space_wraps() {
        let mut log = AccessLog::new();
        log.push_word_big_endian(u32::MAX, false, 0xBEEF);
        assert_eq!(log.as_slice()[1].address, 0);
        assert_eq!(log.as_slice()[1].value, 0xEF);
    }

    #[test]
    fn clearing_empties_the_log() {
        let mut log = AccessLog::new();
        assert!(log.is_empty());
        log.push_word_big_endian(0, false, 0);
        assert!(!log.is_empty());
        log.clear();
        assert!(log.is_empty());
        assert!(log.as_slice().is_empty());
    }
}
