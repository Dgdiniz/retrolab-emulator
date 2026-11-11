//! Pure parser + field-level differ for bsnes-plus trace lines.
//!
//! Both the core's own trace output (`format_trace_line_65816` /
//! `format_trace_line_spc700`) and the bsnes-plus reference logs share
//! the exact same textual layout, so a single parser handles both
//! sides of a comparison. Everything here is allocation-light string
//! work with no dependency on the emulator core, which keeps the unit
//! tests fast and CI-friendly (no ROMs required).
//!
//! ## 65C816 line
//! ```text
//! 008001 stz $4200      [004200] A:0280 X:000a Y:0001 S:01f1 D:0000 DB:00 ..1B.I.. V:  0 H: 50 F: 0
//! ```
//! `pc` is the first six hex digits; the disassembly (plus an optional
//! `[effaddr]` bracket) runs up to `" A:"`; then the `A/X/Y/S/D/DB`
//! registers, the eight status-flag characters, and the `V/H/F` timing
//! columns.
//!
//! ## SPC700 line
//! ```text
//! ..ffc0 mov   x, #$ef           A:00 X:00 Y:00 SP:01ef YA:0000 ......Z.
//! ```
//! `..` bank filler + four hex `pc`, the disassembly up to `" A:"`,
//! the `A/X/Y/SP/YA` registers, and the eight `NVPBHIZC` flag
//! characters. There are no timing columns, so `--skip-timing` is a
//! no-op for the SPC700.

use std::collections::HashSet;

/// Which CPU a trace belongs to. The CLI maps `--cpu snes` to the
/// 5A22 (65C816) at debugger id 0, `--cpu spc700` to the SPC700 at
/// id 1, `--cpu gsu` to the Super FX GSU at id 2, and `--cpu cx4` to
/// the Cx4 HG51B169 (also debugger id 2, GSU and Cx4 are per-cart
/// exclusive so the slot is unambiguous for a loaded ROM).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceCpu {
    Snes,
    Spc700,
    Gsu,
    Hg51b,
}

impl TraceCpu {
    /// Debugger CPU id (`0` = 65C816, `1` = SPC700, `2` = Super FX GSU /
    /// Cx4 HG51B169, the secondary-CPU slot, one chip per cart).
    pub fn cpu_id(self) -> usize {
        match self {
            TraceCpu::Snes => 0,
            TraceCpu::Spc700 => 1,
            TraceCpu::Gsu | TraceCpu::Hg51b => 2,
        }
    }

    /// Human-facing label for summaries.
    pub fn label(self) -> &'static str {
        match self {
            TraceCpu::Snes => "65C816",
            TraceCpu::Spc700 => "SPC700",
            TraceCpu::Gsu => "GSU",
            TraceCpu::Hg51b => "Cx4",
        }
    }

    /// Parse the `--cpu` flag. Accepts a few common aliases so a user
    /// typing `65816` or `smp` is not surprised.
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "snes" | "65816" | "65c816" | "5a22" | "main" | "cpu" => Ok(TraceCpu::Snes),
            "spc700" | "spc" | "smp" | "apu" | "audio" => Ok(TraceCpu::Spc700),
            "gsu" | "superfx" | "super-fx" | "fx" => Ok(TraceCpu::Gsu),
            "cx4" | "hg51b" | "hg51b169" => Ok(TraceCpu::Hg51b),
            other => {
                anyhow::bail!(
                    "unknown --cpu '{other}' (expected 'snes', 'spc700', 'gsu', or 'cx4')"
                )
            }
        }
    }
}

/// One parsed trace line, normalized into comparable fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceLine {
    /// Instruction address (24-bit for 65C816, 16-bit for SPC700).
    pub pc: u32,
    /// Mnemonic + operands, lowercased, with surrounding padding trimmed.
    pub disasm: String,
    /// Effective-address bracket (`[......]`), 65C816 only; `None`
    /// when the addressing mode prints no bracket or for the SPC700.
    pub eff: Option<u32>,
    /// Named registers in display order. Same set/order for a given
    /// CPU, so two lines diff positionally.
    pub regs: Vec<(&'static str, u64)>,
    /// The eight status-flag characters (rendered, not a bitmask).
    pub flags: String,
    /// Timing columns (`V`/`H`/`F`), 65C816 only; empty for the SPC700.
    pub timing: Vec<(&'static str, i64)>,
}

/// A single differing field between two trace lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldDiff {
    pub field: String,
    pub ours: String,
    pub theirs: String,
}

impl TraceLine {
    /// Compare two lines field-by-field. Returns the list of
    /// differences (empty == identical under the active filters).
    ///
    /// - `skip_timing` drops the `V`/`H`/`F` columns (the documented
    ///   discipline for cross-emulator timing models that differ).
    /// - `ignore` holds upper-cased field names to skip entirely
    ///   (e.g. `A`, `X`, `DB`, `P`, `eff`).
    pub fn differs(
        &self,
        other: &TraceLine,
        skip_timing: bool,
        ignore: &HashSet<String>,
    ) -> Vec<FieldDiff> {
        let ignored = |name: &str| ignore.contains(&name.to_ascii_uppercase());
        let mut out = Vec::new();

        if !ignored("PC") && self.pc != other.pc {
            out.push(FieldDiff {
                field: "PC".to_string(),
                ours: format!("{:06x}", self.pc),
                theirs: format!("{:06x}", other.pc),
            });
        }
        if !ignored("DISASM") && self.disasm != other.disasm {
            out.push(FieldDiff {
                field: "disasm".to_string(),
                ours: self.disasm.clone(),
                theirs: other.disasm.clone(),
            });
        }
        if !ignored("EFF") && self.eff != other.eff {
            out.push(FieldDiff {
                field: "eff".to_string(),
                ours: fmt_opt_addr(self.eff),
                theirs: fmt_opt_addr(other.eff),
            });
        }
        for (&(name, ov), &(_, tv)) in self.regs.iter().zip(other.regs.iter()) {
            if !ignored(name) && ov != tv {
                out.push(FieldDiff {
                    field: name.to_string(),
                    ours: format!("{ov:x}"),
                    theirs: format!("{tv:x}"),
                });
            }
        }
        if !ignored("P") && self.flags != other.flags {
            out.push(FieldDiff {
                field: "P".to_string(),
                ours: self.flags.clone(),
                theirs: other.flags.clone(),
            });
        }
        if !skip_timing {
            for (&(name, ov), &(_, tv)) in self.timing.iter().zip(other.timing.iter()) {
                if !ignored(name) && ov != tv {
                    out.push(FieldDiff {
                        field: name.to_string(),
                        ours: ov.to_string(),
                        theirs: tv.to_string(),
                    });
                }
            }
        }
        out
    }
}

fn fmt_opt_addr(a: Option<u32>) -> String {
    match a {
        Some(v) => format!("{v:06x}"),
        None => "none".to_string(),
    }
}

/// Parse a trace line for the given CPU. Returns `None` for blank
/// lines or anything that does not match the expected layout (so the
/// comparison loop can skip stray reference lines defensively).
pub fn parse(cpu: TraceCpu, line: &str) -> Option<TraceLine> {
    match cpu {
        TraceCpu::Snes => parse_65816(line),
        TraceCpu::Spc700 => parse_spc700(line),
        TraceCpu::Gsu => parse_gsu(line),
        TraceCpu::Hg51b => parse_cx4(line),
    }
}

/// Hex value immediately following `label` (stops at the first
/// non-hex-digit). `None` if the label is absent or unfollowed by hex.
fn hex_after(s: &str, label: &str) -> Option<u64> {
    let idx = s.find(label)?;
    let rest = &s[idx + label.len()..];
    let hex: String = rest.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    if hex.is_empty() {
        return None;
    }
    u64::from_str_radix(&hex, 16).ok()
}

/// Decimal value following `label` (after optional spaces, with an
/// optional leading `-`). bsnes pads these columns with spaces
/// (`V:  0`), hence the `trim_start`.
fn dec_after(s: &str, label: &str) -> Option<i64> {
    let idx = s.find(label)?;
    let rest = s[idx + label.len()..].trim_start();
    let (neg, digits_src) = match rest.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, rest),
    };
    let digits: String = digits_src
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return None;
    }
    let v: i64 = digits.parse().ok()?;
    Some(if neg { -v } else { v })
}

/// Split a 65C816 disassembly region into the mnemonic/operand text
/// and an optional trailing `[effaddr]` bracket. Operand brackets
/// (65C816 long-indirect, e.g. `lda [$12]`) always contain a `$`, so
/// they never parse as a plain-hex effective address.
fn split_disasm_eff(region: &str) -> (String, Option<u32>) {
    let region = region.trim();
    if region.ends_with(']') {
        if let Some(open) = region.rfind('[') {
            let inner = &region[open + 1..region.len() - 1];
            if let Ok(addr) = u32::from_str_radix(inner.trim(), 16) {
                return (region[..open].trim_end().to_string(), Some(addr));
            }
        }
    }
    (region.to_string(), None)
}

fn parse_65816(line: &str) -> Option<TraceLine> {
    let line = line.trim_end();
    if line.len() < 7 {
        return None;
    }
    let pc = u32::from_str_radix(line.get(0..6)?, 16).ok()?;
    let a_pos = line.find(" A:")?;
    let (disasm, eff) = split_disasm_eff(line.get(7..a_pos)?);
    let regs_region = &line[a_pos + 1..];

    let a = hex_after(regs_region, "A:")?;
    let x = hex_after(regs_region, "X:")?;
    let y = hex_after(regs_region, "Y:")?;
    let s = hex_after(regs_region, "S:")?;
    let d = hex_after(regs_region, "D:")?;
    let db = hex_after(regs_region, "DB:")?;

    let v_pos = regs_region.find(" V:")?;
    if v_pos < 8 {
        return None;
    }
    let flags = regs_region[v_pos - 8..v_pos].to_string();

    let v = dec_after(regs_region, "V:")?;
    let h = dec_after(regs_region, "H:")?;
    let f = dec_after(regs_region, "F:")?;

    Some(TraceLine {
        pc,
        disasm,
        eff,
        regs: vec![("A", a), ("X", x), ("Y", y), ("S", s), ("D", d), ("DB", db)],
        flags,
        timing: vec![("V", v), ("H", h), ("F", f)],
    })
}

fn parse_spc700(line: &str) -> Option<TraceLine> {
    let line = line.trim_end();
    if line.len() < 7 || !line.starts_with("..") {
        return None;
    }
    let pc = u32::from_str_radix(line.get(2..6)?, 16).ok()?;
    let a_pos = line.find(" A:")?;
    let disasm = line.get(7..a_pos)?.trim().to_string();
    let regs_region = &line[a_pos + 1..];

    let a = hex_after(regs_region, "A:")?;
    let x = hex_after(regs_region, "X:")?;
    let y = hex_after(regs_region, "Y:")?;
    let sp = hex_after(regs_region, "SP:")?;
    let ya = hex_after(regs_region, "YA:")?;

    let trimmed = regs_region.trim_end();
    if trimmed.len() < 8 {
        return None;
    }
    let flags = trimmed[trimmed.len() - 8..].to_string();

    Some(TraceLine {
        pc,
        disasm,
        eff: None,
        regs: vec![("A", a), ("X", x), ("Y", y), ("SP", sp), ("YA", ya)],
        flags,
        timing: Vec::new(),
    })
}

/// Register names for the GSU trace, in the `R0..R15` display order the
/// formatter emits. `'static` so they can live in the [`TraceLine::regs`]
/// tuples without allocation.
const GSU_REG_NAMES: [&str; 16] = [
    "R0", "R1", "R2", "R3", "R4", "R5", "R6", "R7", "R8", "R9", "R10", "R11", "R12", "R13", "R14",
    "R15",
];

/// Parse a Super FX GSU trace line in the layout
/// [`format_trace_line_gsu`](snes1_core::format_trace_line_gsu) emits:
///
/// ```text
/// 008000 iwt r1,#$beef        R0:0000 R1:beef ... R15:0104 SFR:0120 .....1.g....
/// ```
///
/// There is no bsnes-plus GSU reference, so this parses our own format, it
/// makes `compare-trace --cpu gsu` a self-diff (e.g. against a saved
/// regression log). Returns `None` for a line that does not match.
fn parse_gsu(line: &str) -> Option<TraceLine> {
    let line = line.trim_end();
    if line.len() < 7 {
        return None;
    }
    let pc = u32::from_str_radix(line.get(0..6)?, 16).ok()?;
    let r0_pos = line.find(" R0:")?;
    let disasm = line.get(7..r0_pos)?.trim().to_string();
    let regs_region = &line[r0_pos + 1..];

    let mut regs = Vec::with_capacity(17);
    for (n, name) in GSU_REG_NAMES.iter().enumerate() {
        regs.push((*name, hex_after(regs_region, &format!("R{n}:"))?));
    }
    let sfr = hex_after(regs_region, "SFR:")?;
    regs.push(("SFR", sfr));

    // The 12-char status field is the last whitespace-delimited token.
    let flags = regs_region.split_whitespace().last()?.to_string();

    Some(TraceLine {
        pc,
        disasm,
        eff: None,
        regs,
        flags,
        timing: Vec::new(),
    })
}

/// Register names for the Cx4 trace, in the `A P R0..R15` display order the
/// formatter emits. `'static` so they can live in the [`TraceLine::regs`]
/// tuples without allocation.
const CX4_REG_NAMES: [&str; 16] = [
    "R0", "R1", "R2", "R3", "R4", "R5", "R6", "R7", "R8", "R9", "R10", "R11", "R12", "R13", "R14",
    "R15",
];

/// Parse a Cx4 HG51B169 trace line in the layout
/// [`format_trace_line_cx4`](snes1_core::format_trace_line_cx4) emits:
///
/// ```text
/// 000000 ld a,#$12            A:000012 P:0000 R0:000000 ... R15:000000 nzvc
/// ```
///
/// There is no bsnes-plus Cx4 reference, so this parses our own format, it
/// makes `compare-trace --cpu cx4` a self-diff (e.g. against a saved
/// regression log). Returns `None` for a line that does not match.
fn parse_cx4(line: &str) -> Option<TraceLine> {
    let line = line.trim_end();
    if line.len() < 7 {
        return None;
    }
    let pc = u32::from_str_radix(line.get(0..6)?, 16).ok()?;
    let a_pos = line.find(" A:")?;
    let disasm = line.get(7..a_pos)?.trim().to_string();
    let regs_region = &line[a_pos + 1..];

    let mut regs = Vec::with_capacity(18);
    regs.push(("A", hex_after(regs_region, "A:")?));
    regs.push(("P", hex_after(regs_region, "P:")?));
    for (n, name) in CX4_REG_NAMES.iter().enumerate() {
        regs.push((*name, hex_after(regs_region, &format!("R{n}:"))?));
    }

    // The 4-char status field is the last whitespace-delimited token.
    let flags = regs_region.split_whitespace().last()?.to_string();

    Some(TraceLine {
        pc,
        disasm,
        eff: None,
        regs,
        flags,
        timing: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ignore_set(names: &[&str]) -> HashSet<String> {
        names.iter().map(|n| n.to_ascii_uppercase()).collect()
    }

    // Lines in bsnes-plus's trace format, as this tree's tracer writes them
    // for a ROM of our own: its reset handler's `sei` and `stz $4200`, and
    // the SPC700's first instruction in the IPL ROM.
    const LINE_SEI: &str =
        "008000 sei                     A:0000 X:0000 Y:0000 S:01fd D:0000 DB:00 ..1B.I.. V:  0 H:  0 F: 0";
    const LINE_STZ: &str =
        "008001 stz $4200      [004200] A:0000 X:0000 Y:0000 S:01fd D:0000 DB:00 ..1B.I.. V:  0 H:  3 F: 0";
    const LINE_SPC: &str = "..ffc0 mov   x, #$ef           A:00 X:00 Y:00 SP:01ef YA:0000 ......Z.";

    #[test]
    fn parses_65816_no_eff() {
        let p = parse(TraceCpu::Snes, LINE_SEI).expect("parse");
        assert_eq!(p.pc, 0x008000);
        assert_eq!(p.disasm, "sei");
        assert_eq!(p.eff, None);
        assert_eq!(p.regs[0], ("A", 0x0000));
        assert_eq!(p.regs[1], ("X", 0x0000));
        assert_eq!(p.regs[5], ("DB", 0x00));
        assert_eq!(p.flags, "..1B.I..");
        assert_eq!(p.timing, vec![("V", 0), ("H", 0), ("F", 0)]);
    }

    #[test]
    fn parses_65816_with_eff() {
        let p = parse(TraceCpu::Snes, LINE_STZ).expect("parse");
        assert_eq!(p.pc, 0x008001);
        assert_eq!(p.disasm, "stz $4200");
        assert_eq!(p.eff, Some(0x004200));
        assert_eq!(p.timing, vec![("V", 0), ("H", 3), ("F", 0)]);
    }

    #[test]
    fn parses_spc700() {
        let p = parse(TraceCpu::Spc700, LINE_SPC).expect("parse");
        assert_eq!(p.pc, 0x00ffc0);
        assert_eq!(p.disasm, "mov   x, #$ef");
        assert_eq!(p.eff, None);
        assert_eq!(p.regs[0], ("A", 0x00));
        assert_eq!(p.regs[3], ("SP", 0x01ef));
        assert_eq!(p.regs[4], ("YA", 0x0000));
        assert_eq!(p.flags, "......Z.");
        assert!(p.timing.is_empty());
    }

    #[test]
    fn identical_lines_have_no_diff() {
        let a = parse(TraceCpu::Snes, LINE_SEI).unwrap();
        let b = parse(TraceCpu::Snes, LINE_SEI).unwrap();
        assert!(a.differs(&b, false, &HashSet::new()).is_empty());

        let s1 = parse(TraceCpu::Spc700, LINE_SPC).unwrap();
        let s2 = parse(TraceCpu::Spc700, LINE_SPC).unwrap();
        assert!(s1.differs(&s2, false, &HashSet::new()).is_empty());
    }

    #[test]
    fn flipped_register_is_one_field_diff() {
        let ours = parse(TraceCpu::Snes, LINE_SEI).unwrap();
        let mutated = LINE_SEI.replace("X:0000", "X:00ff");
        let theirs = parse(TraceCpu::Snes, &mutated).unwrap();
        let diffs = ours.differs(&theirs, false, &HashSet::new());
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].field, "X");
        assert_eq!(diffs[0].ours, "0");
        assert_eq!(diffs[0].theirs, "ff");
    }

    #[test]
    fn ignored_register_is_dropped() {
        let ours = parse(TraceCpu::Snes, LINE_SEI).unwrap();
        let mutated = LINE_SEI.replace("X:0000", "X:00ff");
        let theirs = parse(TraceCpu::Snes, &mutated).unwrap();
        assert!(ours.differs(&theirs, false, &ignore_set(&["X"])).is_empty());
    }

    #[test]
    fn timing_only_change_is_skipped_under_skip_timing() {
        let ours = parse(TraceCpu::Snes, LINE_SEI).unwrap();
        let mutated = LINE_SEI.replace("H:  0", "H: 99");
        let theirs = parse(TraceCpu::Snes, &mutated).unwrap();
        // Visible without skip_timing...
        assert_eq!(ours.differs(&theirs, false, &HashSet::new()).len(), 1);
        // ...and gone with it.
        assert!(ours.differs(&theirs, true, &HashSet::new()).is_empty());
    }

    #[test]
    fn disasm_difference_is_reported() {
        let ours = parse(TraceCpu::Snes, LINE_STZ).unwrap();
        let mutated = LINE_STZ
            .replace("stz $4200", "stz $4201")
            .replace("[004200]", "[004201]");
        let theirs = parse(TraceCpu::Snes, &mutated).unwrap();
        let diffs = ours.differs(&theirs, false, &HashSet::new());
        let fields: Vec<&str> = diffs.iter().map(|d| d.field.as_str()).collect();
        assert!(fields.contains(&"disasm"));
        assert!(fields.contains(&"eff"));
    }

    #[test]
    fn spc700_flag_change_is_reported() {
        let ours = parse(TraceCpu::Spc700, LINE_SPC).unwrap();
        let mutated = LINE_SPC.replace("......Z.", "N.....Z.");
        let theirs = parse(TraceCpu::Spc700, &mutated).unwrap();
        let diffs = ours.differs(&theirs, false, &HashSet::new());
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].field, "P");
    }

    #[test]
    fn blank_and_garbage_lines_do_not_parse() {
        assert!(parse(TraceCpu::Snes, "").is_none());
        assert!(parse(TraceCpu::Snes, "   ").is_none());
        assert!(parse(TraceCpu::Snes, "not a trace line").is_none());
        assert!(parse(TraceCpu::Spc700, "008000 sei").is_none());
    }

    #[test]
    fn cpu_parse_aliases() {
        assert_eq!(TraceCpu::parse("snes").unwrap(), TraceCpu::Snes);
        assert_eq!(TraceCpu::parse("SPC700").unwrap(), TraceCpu::Spc700);
        assert_eq!(TraceCpu::parse("smp").unwrap(), TraceCpu::Spc700);
        assert_eq!(TraceCpu::parse("gsu").unwrap(), TraceCpu::Gsu);
        assert_eq!(TraceCpu::parse("superfx").unwrap(), TraceCpu::Gsu);
        assert_eq!(TraceCpu::parse("cx4").unwrap(), TraceCpu::Hg51b);
        assert_eq!(TraceCpu::parse("hg51b").unwrap(), TraceCpu::Hg51b);
        assert_eq!(TraceCpu::Snes.cpu_id(), 0);
        assert_eq!(TraceCpu::Spc700.cpu_id(), 1);
        assert_eq!(TraceCpu::Gsu.cpu_id(), 2);
        assert_eq!(TraceCpu::Hg51b.cpu_id(), 2);
        assert_eq!(TraceCpu::Hg51b.label(), "Cx4");
        assert!(TraceCpu::parse("nes").is_err());
    }

    #[test]
    fn parses_cx4_self_format() {
        // A line in the exact `format_trace_line_cx4` layout round-trips
        // through the self-diff parser (no bsnes-plus Cx4 reference exists).
        let line = "000010 ld a,#$12            A:000012 P:0001 R0:000000 R1:0000ab R2:000000 \
                    R3:000000 R4:000000 R5:000000 R6:000000 R7:000000 R8:000000 R9:000000 \
                    R10:000000 R11:000000 R12:000000 R13:000000 R14:000000 R15:000000 n..c";
        let p = parse(TraceCpu::Hg51b, line).expect("parse cx4");
        assert_eq!(p.pc, 0x000010);
        assert_eq!(p.disasm, "ld a,#$12");
        assert_eq!(p.regs[0], ("A", 0x12));
        assert_eq!(p.regs[1], ("P", 0x1));
        assert_eq!(p.regs[3], ("R1", 0xab));
        assert_eq!(p.flags, "n..c");
        assert!(p.eff.is_none());
    }

    #[test]
    fn cx4_register_change_is_one_field_diff() {
        let line = "000010 ld a,#$12            A:000012 P:0001 R0:000000 R1:0000ab R2:000000 \
                    R3:000000 R4:000000 R5:000000 R6:000000 R7:000000 R8:000000 R9:000000 \
                    R10:000000 R11:000000 R12:000000 R13:000000 R14:000000 R15:000000 n..c";
        let ours = parse(TraceCpu::Hg51b, line).unwrap();
        let theirs = parse(TraceCpu::Hg51b, &line.replace("R1:0000ab", "R1:0000ff")).unwrap();
        let diffs = ours.differs(&theirs, false, &HashSet::new());
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].field, "R1");
    }
}
