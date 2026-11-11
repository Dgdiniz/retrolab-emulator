//! Pure parser + field-level differ for MAME-format Genesis trace lines.
//!
//! This module has no dependency on the emulator core: it parses a text
//! trace line (either our own dump or a MAME reference log) into a typed
//! [`TraceLine`] and diffs two of them field by field. The live capture,
//! binary->text formatting, and run loops live in `trace_driver.rs`; the
//! command handlers live under `cmd/`. Keeping the parse/diff logic here
//! makes it unit-testable without a ROM.
//!
//! Two CPUs are supported. The 68000 line carries the optional MAME
//! timing prefix (`TC`/`FR`/`BX`/`BY`); divergence is decided purely on
//! the architectural registers (PC, SR, D0-D7, A0-A7), with the timing
//! columns surfaced only as a "timing-only" classification so a run that
//! matches every register but drifts on cycle count is reported as a
//! match. The Z80 line carries PC + the register pairs + interrupt state.

use std::collections::HashSet;

/// Which CPU a trace line describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceCpu {
    M68k,
    Z80,
}

impl TraceCpu {
    /// Parse a `--cpu` argument. Accepts `m68k`/`68k`/`68000` and `z80`.
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "m68k" | "68k" | "68000" => Ok(TraceCpu::M68k),
            "z80" => Ok(TraceCpu::Z80),
            other => anyhow::bail!("unknown CPU '{other}' (expected m68k or z80)"),
        }
    }

    /// Human-readable label for report headers.
    pub fn label(self) -> &'static str {
        match self {
            TraceCpu::M68k => "68000",
            TraceCpu::Z80 => "Z80",
        }
    }

    /// The `DebuggerCore` CPU id (68000 = 0, Z80 = 1).
    pub fn cpu_id(self) -> usize {
        match self {
            TraceCpu::M68k => 0,
            TraceCpu::Z80 => 1,
        }
    }
}

/// One differing field between two trace lines, for the divergence report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldDiff {
    pub field: String,
    pub ours: String,
    pub theirs: String,
}

/// A parsed 68000 trace entry.
///
/// Works for both our dump and the MAME reference: the optional MAME
/// timing fields (`TC`/`FR`/`BX`/`BY`) are parsed when present and left
/// `None` otherwise. PC is stored full-width but compared masked to 24
/// bits (the 68000 address bus width).
#[derive(Default, Clone)]
pub struct TraceEntry {
    pub pc: u32,
    pub sr: u16,
    pub d: [u32; 8],
    pub a: [u32; 8],
    pub total_cycles: Option<u64>,
    pub beam_y: Option<u32>,
    pub beam_x: Option<u32>,
    pub frame: Option<u32>,
}

impl TraceEntry {
    /// Parse a 68000 trace line. Supports the bare register format
    /// (`PC:XXXXXX SR:XXXX D0:... | disasm`) and the MAME extended format
    /// (`TC:... FR:... BX:... BY:... PC:... | disasm`). Returns `None`
    /// for any line without a `PC` field (blank lines, interrupt
    /// notifications, etc.).
    pub fn parse(line: &str) -> Option<Self> {
        let regs_part = line.split('|').next()?;
        let mut entry = TraceEntry::default();
        let mut found_pc = false;

        for part in regs_part.split_whitespace() {
            if let Some((name, value)) = part.split_once(':') {
                match name {
                    "TC" => entry.total_cycles = u64::from_str_radix(value, 16).ok(),
                    "FR" => entry.frame = u32::from_str_radix(value, 16).ok(),
                    "BX" => entry.beam_x = u32::from_str_radix(value, 16).ok(),
                    "BY" => entry.beam_y = u32::from_str_radix(value, 16).ok(),
                    _ => {
                        if let Ok(val) = u32::from_str_radix(value, 16) {
                            match name {
                                "PC" => {
                                    entry.pc = val;
                                    found_pc = true;
                                }
                                "SR" => entry.sr = val as u16,
                                "D0" => entry.d[0] = val,
                                "D1" => entry.d[1] = val,
                                "D2" => entry.d[2] = val,
                                "D3" => entry.d[3] = val,
                                "D4" => entry.d[4] = val,
                                "D5" => entry.d[5] = val,
                                "D6" => entry.d[6] = val,
                                "D7" => entry.d[7] = val,
                                "A0" => entry.a[0] = val,
                                "A1" => entry.a[1] = val,
                                "A2" => entry.a[2] = val,
                                "A3" => entry.a[3] = val,
                                "A4" => entry.a[4] = val,
                                "A5" => entry.a[5] = val,
                                "A6" => entry.a[6] = val,
                                "A7" => entry.a[7] = val,
                                _ => {}
                            }
                        }
                    }
                }
            }
        }

        if found_pc {
            Some(entry)
        } else {
            None
        }
    }

    /// Whether the architectural registers (PC, SR, D, A) differ, honoring
    /// the ignore set. PC is masked to 24 bits.
    pub fn regs_differ(&self, other: &Self, ignore: &HashSet<String>) -> bool {
        if !ignore.contains("PC") && (self.pc & 0x00FF_FFFF) != (other.pc & 0x00FF_FFFF) {
            return true;
        }
        if !ignore.contains("SR") && self.sr != other.sr {
            return true;
        }
        for i in 0..8 {
            if !ignore.contains(&format!("D{i}")) && self.d[i] != other.d[i] {
                return true;
            }
        }
        for i in 0..8 {
            if !ignore.contains(&format!("A{i}")) && self.a[i] != other.a[i] {
                return true;
            }
        }
        false
    }

    /// Whether only the timing columns differ (every register matches but a
    /// present `TC`/`BX`/`BY`/`FR` value differs). Used to classify a
    /// register-clean line for the `--skip-timing` statistic.
    pub fn only_timing_differs(&self, other: &Self, ignore: &HashSet<String>) -> bool {
        if self.regs_differ(other, ignore) {
            return false;
        }
        let pairs = [
            (self.total_cycles, other.total_cycles),
            (
                self.beam_y.map(|v| v as u64),
                other.beam_y.map(|v| v as u64),
            ),
            (
                self.beam_x.map(|v| v as u64),
                other.beam_x.map(|v| v as u64),
            ),
            (self.frame.map(|v| v as u64), other.frame.map(|v| v as u64)),
        ];
        pairs
            .iter()
            .any(|(a, b)| matches!((a, b), (Some(x), Some(y)) if x != y))
    }

    /// Per-field register differences for the divergence report.
    pub fn register_diffs(&self, other: &Self, ignore: &HashSet<String>) -> Vec<FieldDiff> {
        let mut diffs = Vec::new();
        if !ignore.contains("PC") && (self.pc & 0x00FF_FFFF) != (other.pc & 0x00FF_FFFF) {
            diffs.push(FieldDiff {
                field: "PC".to_string(),
                ours: format!("{:06X}", self.pc & 0x00FF_FFFF),
                theirs: format!("{:06X}", other.pc & 0x00FF_FFFF),
            });
        }
        if !ignore.contains("SR") && self.sr != other.sr {
            diffs.push(FieldDiff {
                field: "SR".to_string(),
                ours: format!("{:04X} ({})", self.sr, format_sr_flags(self.sr)),
                theirs: format!("{:04X} ({})", other.sr, format_sr_flags(other.sr)),
            });
        }
        for i in 0..8 {
            let name = format!("D{i}");
            if !ignore.contains(&name) && self.d[i] != other.d[i] {
                diffs.push(FieldDiff {
                    field: name,
                    ours: format!("{:08X}", self.d[i]),
                    theirs: format!("{:08X}", other.d[i]),
                });
            }
        }
        for i in 0..8 {
            let name = format!("A{i}");
            if !ignore.contains(&name) && self.a[i] != other.a[i] {
                diffs.push(FieldDiff {
                    field: name,
                    ours: format!("{:08X}", self.a[i]),
                    theirs: format!("{:08X}", other.a[i]),
                });
            }
        }
        diffs
    }
}

/// A parsed Z80 trace entry (PC + register pairs + interrupt state).
#[derive(Default, Clone)]
pub struct Z80TraceEntry {
    pub pc: u16,
    pub af: u16,
    pub bc: u16,
    pub de: u16,
    pub hl: u16,
    pub ix: u16,
    pub iy: u16,
    pub sp: u16,
    pub i_reg: u8,
    pub r_reg: u8,
    pub im: u8,
    pub iff1: bool,
    pub iff2: bool,
    pub halt: bool,
}

impl Z80TraceEntry {
    /// Parse a Z80 MAME trace line:
    /// `TC:.. FR:.. BX:.. BY:.. PC:XXXX AF:XXXX .. HALT:X | disasm`.
    /// The timing columns are skipped. Returns `None` without a `PC`.
    pub fn parse(line: &str) -> Option<Self> {
        let regs_part = line.split('|').next()?;
        let mut entry = Z80TraceEntry::default();
        let mut found_pc = false;

        for part in regs_part.split_whitespace() {
            if let Some((name, value)) = part.split_once(':') {
                match name {
                    "TC" | "FR" | "BX" | "BY" => { /* timing columns: ignored */ }
                    "PC" => {
                        entry.pc = u16::from_str_radix(value, 16).ok()?;
                        found_pc = true;
                    }
                    "AF" => entry.af = u16::from_str_radix(value, 16).ok()?,
                    "BC" => entry.bc = u16::from_str_radix(value, 16).ok()?,
                    "DE" => entry.de = u16::from_str_radix(value, 16).ok()?,
                    "HL" => entry.hl = u16::from_str_radix(value, 16).ok()?,
                    "IX" => entry.ix = u16::from_str_radix(value, 16).ok()?,
                    "IY" => entry.iy = u16::from_str_radix(value, 16).ok()?,
                    "SP" => entry.sp = u16::from_str_radix(value, 16).ok()?,
                    "I" => entry.i_reg = u8::from_str_radix(value, 16).ok()?,
                    "R" => entry.r_reg = u8::from_str_radix(value, 16).ok()?,
                    "IM" => entry.im = value.parse().ok()?,
                    "IFF1" => entry.iff1 = value == "1",
                    "IFF2" => entry.iff2 = value == "1",
                    "HALT" => entry.halt = value == "1",
                    _ => {}
                }
            }
        }

        if found_pc {
            Some(entry)
        } else {
            None
        }
    }

    /// Whether any non-ignored register/flag differs.
    pub fn regs_differ(&self, other: &Self, ignore: &HashSet<String>) -> bool {
        !self.register_diffs(other, ignore).is_empty()
    }

    /// Per-field differences for the divergence report.
    pub fn register_diffs(&self, other: &Self, ignore: &HashSet<String>) -> Vec<FieldDiff> {
        let mut diffs = Vec::new();

        let mut push16 = |name: &str, a: u16, b: u16| {
            if !ignore.contains(name) && a != b {
                diffs.push(FieldDiff {
                    field: name.to_string(),
                    ours: format!("{a:04X}"),
                    theirs: format!("{b:04X}"),
                });
            }
        };
        push16("PC", self.pc, other.pc);
        push16("AF", self.af, other.af);
        push16("BC", self.bc, other.bc);
        push16("DE", self.de, other.de);
        push16("HL", self.hl, other.hl);
        push16("IX", self.ix, other.ix);
        push16("IY", self.iy, other.iy);
        push16("SP", self.sp, other.sp);

        let mut push8 = |name: &str, a: u8, b: u8| {
            if !ignore.contains(name) && a != b {
                diffs.push(FieldDiff {
                    field: name.to_string(),
                    ours: format!("{a:02X}"),
                    theirs: format!("{b:02X}"),
                });
            }
        };
        push8("I", self.i_reg, other.i_reg);
        push8("R", self.r_reg, other.r_reg);

        if !ignore.contains("IM") && self.im != other.im {
            diffs.push(FieldDiff {
                field: "IM".to_string(),
                ours: self.im.to_string(),
                theirs: other.im.to_string(),
            });
        }

        let mut push_bool = |name: &str, a: bool, b: bool| {
            if !ignore.contains(name) && a != b {
                diffs.push(FieldDiff {
                    field: name.to_string(),
                    ours: (a as u8).to_string(),
                    theirs: (b as u8).to_string(),
                });
            }
        };
        push_bool("IFF1", self.iff1, other.iff1);
        push_bool("IFF2", self.iff2, other.iff2);
        push_bool("HALT", self.halt, other.halt);

        diffs
    }
}

/// A parsed trace line for one of the two CPUs. The comparator keys on
/// this so the same diff logic serves both the live `compare-trace` and
/// the offline `compare`.
#[derive(Clone)]
pub enum TraceLine {
    M68k(TraceEntry),
    Z80(Z80TraceEntry),
}

impl TraceLine {
    /// Parse `line` as a trace entry for `cpu`, or `None` if it has no PC.
    pub fn parse(cpu: TraceCpu, line: &str) -> Option<Self> {
        match cpu {
            TraceCpu::M68k => TraceEntry::parse(line).map(TraceLine::M68k),
            TraceCpu::Z80 => Z80TraceEntry::parse(line).map(TraceLine::Z80),
        }
    }

    /// The program counter, for resync notes.
    pub fn pc(&self) -> u32 {
        match self {
            TraceLine::M68k(e) => e.pc & 0x00FF_FFFF,
            TraceLine::Z80(e) => e.pc as u32,
        }
    }

    /// Whether the architectural registers differ (the divergence test).
    /// Two lines for different CPUs are always considered to differ.
    pub fn regs_differ(&self, other: &Self, ignore: &HashSet<String>) -> bool {
        match (self, other) {
            (TraceLine::M68k(a), TraceLine::M68k(b)) => a.regs_differ(b, ignore),
            (TraceLine::Z80(a), TraceLine::Z80(b)) => a.regs_differ(b, ignore),
            _ => true,
        }
    }

    /// Per-field register differences for the divergence report.
    pub fn register_diffs(&self, other: &Self, ignore: &HashSet<String>) -> Vec<FieldDiff> {
        match (self, other) {
            (TraceLine::M68k(a), TraceLine::M68k(b)) => a.register_diffs(b, ignore),
            (TraceLine::Z80(a), TraceLine::Z80(b)) => a.register_diffs(b, ignore),
            _ => Vec::new(),
        }
    }

    /// Whether the registers match but a timing column differs (68000
    /// only; always false for the Z80, which has no timing columns here).
    pub fn only_timing_differs(&self, other: &Self, ignore: &HashSet<String>) -> bool {
        match (self, other) {
            (TraceLine::M68k(a), TraceLine::M68k(b)) => a.only_timing_differs(b, ignore),
            _ => false,
        }
    }
}

/// Format the 68000 status-register CCR bits as `XNZVC` (a set bit shows
/// its letter, a clear bit shows `.`).
pub fn format_sr_flags(sr: u16) -> String {
    let bit = |mask: u16, ch: char| if sr & mask != 0 { ch } else { '.' };
    format!(
        "{}{}{}{}{}",
        bit(0x10, 'X'),
        bit(0x08, 'N'),
        bit(0x04, 'Z'),
        bit(0x02, 'V'),
        bit(0x01, 'C')
    )
}

/// Parse a comma-separated `--ignore` list into an upper-cased set.
pub fn parse_ignore(spec: Option<&str>) -> HashSet<String> {
    spec.map(|s| {
        s.split(',')
            .map(|r| r.trim().to_ascii_uppercase())
            .filter(|r| !r.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_ignore() -> HashSet<String> {
        HashSet::new()
    }

    #[test]
    fn parse_mame_m68k_line_with_timing() {
        let line = "TC:0000000000000022 FR:000000 BX:0054 BY:00E0 PC:000200 SR:2700 D0:00000001 D1:00000000 D2:00000000 D3:00000000 D4:00000000 D5:00000000 D6:00000000 D7:00000000 A0:00000000 A1:00000000 A2:00000000 A3:00000000 A4:00000000 A5:00000000 A6:00000000 A7:FFFFFF00 | 000200: tst.l $a10008.l";
        let e = TraceEntry::parse(line).expect("parses");
        assert_eq!(e.pc, 0x000200);
        assert_eq!(e.sr, 0x2700);
        assert_eq!(e.d[0], 1);
        assert_eq!(e.a[7], 0xFFFF_FF00);
        assert_eq!(e.total_cycles, Some(0x22));
        assert_eq!(e.beam_y, Some(0xE0));
    }

    #[test]
    fn parse_bare_m68k_line_without_timing() {
        let line = "PC:000206 SR:2708 D0:00000000 D1:00000000 D2:00000000 D3:00000000 D4:00000000 D5:00000000 D6:00000000 D7:00000000 A0:00000000 A1:00000000 A2:00000000 A3:00000000 A4:00000000 A5:00000000 A6:00000000 A7:00FFFE00 | TST.L $00A10008";
        let e = TraceEntry::parse(line).expect("parses");
        assert_eq!(e.pc, 0x000206);
        assert_eq!(e.total_cycles, None);
    }

    #[test]
    fn non_pc_lines_are_skipped() {
        assert!(TraceEntry::parse("").is_none());
        assert!(TraceEntry::parse("(interrupted at 000932, IRQ 6)").is_none());
        assert!(Z80TraceEntry::parse("").is_none());
    }

    #[test]
    fn pc_compared_masked_to_24_bits() {
        let a = TraceEntry {
            pc: 0xFF00_0200,
            ..Default::default()
        };
        let b = TraceEntry {
            pc: 0x0000_0200,
            ..Default::default()
        };
        assert!(!a.regs_differ(&b, &no_ignore()));
    }

    #[test]
    fn register_diff_lists_changed_fields() {
        let a = TraceEntry {
            pc: 0x200,
            d: [1, 0, 0, 0, 0, 0, 0, 0],
            ..Default::default()
        };
        let b = TraceEntry {
            pc: 0x200,
            d: [2, 0, 0, 0, 0, 0, 0, 0],
            ..Default::default()
        };
        let diffs = a.register_diffs(&b, &no_ignore());
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].field, "D0");
        assert_eq!(diffs[0].ours, "00000001");
        assert_eq!(diffs[0].theirs, "00000002");
    }

    #[test]
    fn ignore_set_suppresses_named_fields() {
        let a = TraceEntry {
            a: [0, 0, 0, 0, 0, 0, 0, 0x100],
            ..Default::default()
        };
        let b = TraceEntry {
            a: [0, 0, 0, 0, 0, 0, 0, 0x200],
            ..Default::default()
        };
        let mut ignore = HashSet::new();
        ignore.insert("A7".to_string());
        assert!(!a.regs_differ(&b, &ignore));
    }

    #[test]
    fn only_timing_differs_when_regs_match_but_cycles_drift() {
        let a = TraceEntry {
            pc: 0x200,
            total_cycles: Some(100),
            ..Default::default()
        };
        let b = TraceEntry {
            pc: 0x200,
            total_cycles: Some(110),
            ..Default::default()
        };
        assert!(a.only_timing_differs(&b, &no_ignore()));
        assert!(!a.regs_differ(&b, &no_ignore()));
    }

    #[test]
    fn timing_only_false_when_a_register_also_differs() {
        let a = TraceEntry {
            pc: 0x200,
            total_cycles: Some(100),
            ..Default::default()
        };
        let b = TraceEntry {
            pc: 0x204,
            total_cycles: Some(110),
            ..Default::default()
        };
        assert!(!a.only_timing_differs(&b, &no_ignore()));
    }

    #[test]
    fn parse_z80_line_skips_timing_columns() {
        let line = "TC:0000000000001000 FR:000001 BX:0000 BY:0000 PC:0008 AF:0040 BC:0000 DE:0000 HL:0000 IX:0000 IY:0000 SP:FFFE I:00 R:02 IM:1 IFF1:1 IFF2:1 HALT:0 | ei";
        let e = Z80TraceEntry::parse(line).expect("parses");
        assert_eq!(e.pc, 0x0008);
        assert_eq!(e.af, 0x0040);
        assert_eq!(e.r_reg, 0x02);
        assert_eq!(e.im, 1);
        assert!(e.iff1);
        assert!(!e.halt);
    }

    #[test]
    fn z80_register_diff_reports_flags() {
        let a = Z80TraceEntry {
            pc: 0x10,
            iff1: true,
            ..Default::default()
        };
        let b = Z80TraceEntry {
            pc: 0x10,
            iff1: false,
            ..Default::default()
        };
        let diffs = a.register_diffs(&b, &no_ignore());
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].field, "IFF1");
    }

    #[test]
    fn trace_cpu_parse_accepts_aliases() {
        assert_eq!(TraceCpu::parse("m68k").unwrap(), TraceCpu::M68k);
        assert_eq!(TraceCpu::parse("68000").unwrap(), TraceCpu::M68k);
        assert_eq!(TraceCpu::parse("Z80").unwrap(), TraceCpu::Z80);
        assert!(TraceCpu::parse("spc700").is_err());
    }

    #[test]
    fn trace_line_pc_masks_m68k() {
        let line = TraceLine::M68k(TraceEntry {
            pc: 0xAB00_0200,
            ..Default::default()
        });
        assert_eq!(line.pc(), 0x0000_0200);
    }

    #[test]
    fn sr_flags_render() {
        assert_eq!(format_sr_flags(0x0000), ".....");
        assert_eq!(format_sr_flags(0x001F), "XNZVC");
        assert_eq!(format_sr_flags(0x0008), ".N...");
    }

    #[test]
    fn parse_ignore_uppercases_and_trims() {
        let set = parse_ignore(Some(" sr , a7 ,, d0 "));
        assert!(set.contains("SR"));
        assert!(set.contains("A7"));
        assert!(set.contains("D0"));
        assert_eq!(set.len(), 3);
        assert!(parse_ignore(None).is_empty());
    }
}
