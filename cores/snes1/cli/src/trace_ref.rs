//! The parsers of bsnes-plus-format SNES trace lines into the [`TraceLine`]
//! the shared comparison reads. The core's own lines and the bsnes-plus
//! reference logs share one layout, so one parser reads both sides.
//!
//! ## 65C816 line
//! ```text
//! 008001 stz $4200      [004200] A:0280 X:000a Y:0001 S:01f1 D:0000 DB:00 ..1B.I.. V:  0 H: 50 F: 0
//! ```
//! `pc` is the first six hex digits; the disassembly (with an `[effaddr]`
//! bracket when the addressing mode has one) runs up to `" A:"`; then the
//! `A/X/Y/S/D/DB` registers, the eight status-flag characters, and the
//! `V/H/F` timing columns.
//!
//! ## SPC700 line
//! ```text
//! ..ffc0 mov   x, #$ef           A:00 X:00 Y:00 SP:01ef YA:0000 ......Z.
//! ```
//! `..` and four hex `pc`, the disassembly up to `" A:"`, the `A/X/Y/SP/YA`
//! registers, and the eight `NVPBHIZC` flag characters; no timing columns.
//!
//! The Super FX GSU and the Cx4 have no bsnes-plus reference: their parsers
//! read our own format, so `compare-trace --cpu gsu|cx4` diffs a run against
//! a saved log of ours.

use retro_cli::trace::line::{
    dec_after, hex_after, shown_hex, shown_signed, FieldValue, TraceField, TraceLine,
};

/// The program counter, the disassembly and the effective address, the
/// first fields of every line.
fn head(pc: u32, disasm: String, eff: FieldValue) -> Vec<TraceField> {
    vec![
        TraceField::number("PC", pc as u64, shown_hex::<6>),
        TraceField::text("disasm", disasm),
        TraceField::of("eff", eff, shown_hex::<6>),
    ]
}

/// Split a 65C816 disassembly into its text and an optional trailing
/// `[effaddr]` bracket. A bracketed operand (`lda [$12]`) holds a `$`, so it
/// never reads as a plain-hex effective address.
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

/// Parse a 65C816 line, or `None` for a line that does not have the layout.
pub fn parse_65816(line: &str) -> Option<TraceLine> {
    let line = line.trim_end();
    if line.len() < 7 {
        return None;
    }
    let pc = u32::from_str_radix(line.get(0..6)?, 16).ok()?;
    let a_pos = line.find(" A:")?;
    let (disasm, eff) = split_disasm_eff(line.get(7..a_pos)?);
    let regs = &line[a_pos + 1..];

    let mut fields = head(
        pc,
        disasm,
        eff.map_or(FieldValue::Empty, |e| FieldValue::Number(e as u64)),
    );
    push_registers(
        &mut fields,
        regs,
        &[
            ("A", "A:"),
            ("X", "X:"),
            ("Y", "Y:"),
            ("S", "S:"),
            ("D", "D:"),
            ("DB", "DB:"),
        ],
    )?;
    let v_pos = regs.find(" V:")?;
    if v_pos < 8 {
        return None;
    }
    fields.push(TraceField::text("P", regs[v_pos - 8..v_pos].to_string()));
    for (name, label) in [("V", "V:"), ("H", "H:"), ("F", "F:")] {
        let value = dec_after(regs, label)?;
        fields.push(TraceField::number(name, value as u64, shown_signed).as_timing());
    }
    Some(TraceLine { pc, fields })
}

/// Parse an SPC700 line, or `None` for a line that does not have the layout.
pub fn parse_spc700(line: &str) -> Option<TraceLine> {
    let line = line.trim_end();
    if line.len() < 7 || !line.starts_with("..") {
        return None;
    }
    let pc = u32::from_str_radix(line.get(2..6)?, 16).ok()?;
    let a_pos = line.find(" A:")?;
    let disasm = line.get(7..a_pos)?.trim().to_string();
    let regs = &line[a_pos + 1..];

    let mut fields = head(pc, disasm, FieldValue::Empty);
    push_registers(
        &mut fields,
        regs,
        &[
            ("A", "A:"),
            ("X", "X:"),
            ("Y", "Y:"),
            ("SP", "SP:"),
            ("YA", "YA:"),
        ],
    )?;
    let trimmed = regs.trim_end();
    if trimmed.len() < 8 {
        return None;
    }
    fields.push(TraceField::text(
        "P",
        trimmed[trimmed.len() - 8..].to_string(),
    ));
    Some(TraceLine { pc, fields })
}

/// The GSU's and the Cx4's sixteen registers, with their labels.
const REGISTERS: [(&str, &str); 16] = [
    ("R0", "R0:"),
    ("R1", "R1:"),
    ("R2", "R2:"),
    ("R3", "R3:"),
    ("R4", "R4:"),
    ("R5", "R5:"),
    ("R6", "R6:"),
    ("R7", "R7:"),
    ("R8", "R8:"),
    ("R9", "R9:"),
    ("R10", "R10:"),
    ("R11", "R11:"),
    ("R12", "R12:"),
    ("R13", "R13:"),
    ("R14", "R14:"),
    ("R15", "R15:"),
];

/// The hexadecimal registers `labels` names, read from `regs` in that
/// order; `None` when one is missing.
fn push_registers(
    fields: &mut Vec<TraceField>,
    regs: &str,
    labels: &[(&'static str, &str)],
) -> Option<()> {
    for &(name, label) in labels {
        fields.push(TraceField::number(
            name,
            hex_after(regs, label)?,
            shown_hex::<0>,
        ));
    }
    Some(())
}

/// Parse a Super FX GSU line in the layout
/// [`format_trace_line_gsu`](snes1_core::format_trace_line_gsu) writes:
///
/// ```text
/// 008000 iwt r1,#$beef        R0:0000 R1:beef ... R15:0104 SFR:0120 .....1.g....
/// ```
pub fn parse_gsu(line: &str) -> Option<TraceLine> {
    let line = line.trim_end();
    if line.len() < 7 {
        return None;
    }
    let pc = u32::from_str_radix(line.get(0..6)?, 16).ok()?;
    let r0_pos = line.find(" R0:")?;
    let disasm = line.get(7..r0_pos)?.trim().to_string();
    let regs = &line[r0_pos + 1..];

    let mut fields = head(pc, disasm, FieldValue::Empty);
    push_registers(&mut fields, regs, &REGISTERS)?;
    fields.push(TraceField::number(
        "SFR",
        hex_after(regs, "SFR:")?,
        shown_hex::<0>,
    ));
    // The 12-character status field is the last word.
    fields.push(TraceField::text(
        "P",
        regs.split_whitespace().last()?.to_string(),
    ));
    Some(TraceLine { pc, fields })
}

/// Parse a Cx4 HG51B169 line in the layout
/// [`format_trace_line_cx4`](snes1_core::format_trace_line_cx4) writes:
///
/// ```text
/// 000000 ld a,#$12            A:000012 P:0000 R0:000000 ... R15:000000 nzvc
/// ```
pub fn parse_cx4(line: &str) -> Option<TraceLine> {
    let line = line.trim_end();
    if line.len() < 7 {
        return None;
    }
    let pc = u32::from_str_radix(line.get(0..6)?, 16).ok()?;
    let a_pos = line.find(" A:")?;
    let disasm = line.get(7..a_pos)?.trim().to_string();
    let regs = &line[a_pos + 1..];

    let mut fields = head(pc, disasm, FieldValue::Empty);
    push_registers(&mut fields, regs, &[("A", "A:"), ("P", "P:")])?;
    push_registers(&mut fields, regs, &REGISTERS)?;
    // The 4-character status field is the last word.
    fields.push(TraceField::text(
        "P",
        regs.split_whitespace().last()?.to_string(),
    ));
    Some(TraceLine { pc, fields })
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_cli::trace::compare::{Comparator, CompareRules};
    use std::collections::HashSet;
    use std::io::Cursor;

    fn ignore_set(names: &[&str]) -> HashSet<String> {
        names.iter().map(|n| n.to_ascii_uppercase()).collect()
    }

    fn value(line: &TraceLine, name: &str) -> FieldValue {
        line.field(name).expect("field").value.clone()
    }

    // Lines in bsnes-plus's trace format, as this tree's tracer writes them
    // for a ROM of our own: its reset handler's `sei` and `stz $4200`, and
    // the SPC700's first instruction in the IPL ROM.
    const LINE_SEI: &str =
        "008000 sei                     A:0000 X:0000 Y:0000 S:01fd D:0000 DB:00 ..1B.I.. V:  0 H:  0 F: 0";
    const LINE_STZ: &str =
        "008001 stz $4200      [004200] A:0000 X:0000 Y:0000 S:01fd D:0000 DB:00 ..1B.I.. V:  0 H:  3 F: 0";
    const LINE_SPC: &str = "..ffc0 mov   x, #$ef           A:00 X:00 Y:00 SP:01ef YA:0000 ......Z.";
    const LINE_CX4: &str = "000010 ld a,#$12            A:000012 P:0001 R0:000000 R1:0000ab \
        R2:000000 R3:000000 R4:000000 R5:000000 R6:000000 R7:000000 R8:000000 R9:000000 \
        R10:000000 R11:000000 R12:000000 R13:000000 R14:000000 R15:000000 n..c";

    #[test]
    fn a_65816_line_without_an_effective_address_reads() {
        let p = parse_65816(LINE_SEI).expect("parse");
        assert_eq!(p.pc, 0x008000);
        assert_eq!(value(&p, "disasm"), FieldValue::Text("sei".to_string()));
        assert_eq!(value(&p, "eff"), FieldValue::Empty);
        assert_eq!(value(&p, "X"), FieldValue::Number(0));
        assert_eq!(value(&p, "DB"), FieldValue::Number(0));
        assert_eq!(value(&p, "P"), FieldValue::Text("..1B.I..".to_string()));
        assert_eq!(value(&p, "H"), FieldValue::Number(0));
        assert!(p.field("V").unwrap().timing);
    }

    #[test]
    fn a_65816_line_with_an_effective_address_reads() {
        let p = parse_65816(LINE_STZ).expect("parse");
        assert_eq!(p.pc, 0x008001);
        assert_eq!(
            value(&p, "disasm"),
            FieldValue::Text("stz $4200".to_string())
        );
        assert_eq!(value(&p, "eff"), FieldValue::Number(0x004200));
        assert_eq!(value(&p, "H"), FieldValue::Number(3));
    }

    #[test]
    fn an_spc700_line_reads() {
        let p = parse_spc700(LINE_SPC).expect("parse");
        assert_eq!(p.pc, 0x00ffc0);
        assert_eq!(
            value(&p, "disasm"),
            FieldValue::Text("mov   x, #$ef".to_string())
        );
        assert_eq!(value(&p, "SP"), FieldValue::Number(0x01ef));
        assert_eq!(value(&p, "YA"), FieldValue::Number(0));
        assert_eq!(value(&p, "P"), FieldValue::Text("......Z.".to_string()));
        assert!(p.fields.iter().all(|f| !f.timing));
    }

    #[test]
    fn identical_lines_have_no_difference() {
        let none = HashSet::new();
        let a = parse_65816(LINE_SEI).unwrap();
        assert!(!a.differs_from(&parse_65816(LINE_SEI).unwrap(), false, &none));
        let s = parse_spc700(LINE_SPC).unwrap();
        assert!(!s.differs_from(&parse_spc700(LINE_SPC).unwrap(), false, &none));
    }

    #[test]
    fn a_flipped_register_is_one_difference_as_the_report_always_showed_it() {
        let ours = parse_65816(LINE_SEI).unwrap();
        let theirs = parse_65816(&LINE_SEI.replace("X:0000", "X:00ff")).unwrap();
        let diffs = ours.differences(&theirs, false, &HashSet::new());
        assert_eq!(diffs.len(), 1);
        assert_eq!(
            (
                diffs[0].field,
                diffs[0].ours.as_str(),
                diffs[0].theirs.as_str()
            ),
            ("X", "0", "ff")
        );
        assert!(!ours.differs_from(&theirs, false, &ignore_set(&["X"])));
    }

    #[test]
    fn a_timing_difference_takes_part_unless_skipped() {
        let ours = parse_65816(LINE_SEI).unwrap();
        let theirs = parse_65816(&LINE_SEI.replace("H:  0", "H: 99")).unwrap();
        assert_eq!(ours.differences(&theirs, false, &HashSet::new()).len(), 1);
        assert!(!ours.differs_from(&theirs, true, &HashSet::new()));
    }

    #[test]
    fn a_disassembly_difference_is_reported() {
        let ours = parse_65816(LINE_STZ).unwrap();
        let mutated = LINE_STZ
            .replace("stz $4200", "stz $4201")
            .replace("[004200]", "[004201]");
        let diffs = ours.differences(&parse_65816(&mutated).unwrap(), false, &HashSet::new());
        let fields: Vec<&str> = diffs.iter().map(|d| d.field).collect();
        assert_eq!(fields, vec!["disasm", "eff"]);
        assert_eq!(diffs[1].ours, "004200");
    }

    #[test]
    fn an_spc700_flag_change_is_reported() {
        let ours = parse_spc700(LINE_SPC).unwrap();
        let theirs = parse_spc700(&LINE_SPC.replace("......Z.", "N.....Z.")).unwrap();
        let diffs = ours.differences(&theirs, false, &HashSet::new());
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].field, "P");
    }

    #[test]
    fn blank_and_garbage_lines_do_not_parse() {
        assert!(parse_65816("").is_none());
        assert!(parse_65816("   ").is_none());
        assert!(parse_65816("not a trace line").is_none());
        assert!(parse_spc700("008000 sei").is_none());
    }

    #[test]
    fn a_cx4_line_reads_and_a_register_change_is_one_difference() {
        let p = parse_cx4(LINE_CX4).expect("parse cx4");
        assert_eq!(p.pc, 0x000010);
        assert_eq!(
            value(&p, "disasm"),
            FieldValue::Text("ld a,#$12".to_string())
        );
        assert_eq!(value(&p, "A"), FieldValue::Number(0x12));
        assert_eq!(value(&p, "R1"), FieldValue::Number(0xab));
        let theirs = parse_cx4(&LINE_CX4.replace("R1:0000ab", "R1:0000ff")).unwrap();
        let diffs = p.differences(&theirs, false, &HashSet::new());
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].field, "R1");
    }

    // The shared comparator on bsnes-plus lines.

    /// A 65C816 line; only the program counter, the disassembly and `A`
    /// vary, which is all the resync keys on here, the timing skipped.
    fn tl(pc: u32, disasm: &str, a: u16) -> String {
        format!(
            "{pc:06x} {disasm:<22} A:{a:04x} X:0000 Y:0000 S:01ff D:0000 DB:00 ..M..I.C V:  0 H:  0 F: 0"
        )
    }

    fn make_cmp(reference: &str, budget: usize) -> Comparator<Cursor<Vec<u8>>, Vec<u8>> {
        let rules = CompareRules {
            parse: parse_65816,
            skip_timing: true,
            ignore: HashSet::new(),
            max_errors: 1,
        };
        Comparator::new(
            rules,
            Cursor::new(reference.as_bytes().to_vec()),
            Vec::new(),
            budget,
        )
    }

    #[test]
    fn an_exact_match_takes_each_line_without_divergence() {
        let reference = format!("{}\n{}\n", tl(0x8000, "nop", 0), tl(0x8002, "nop", 0));
        let mut c = make_cmp(&reference, 0);
        assert!(c.feed(&tl(0x8000, "nop", 0)).unwrap().is_continue());
        assert!(c.feed(&tl(0x8002, "nop", 0)).unwrap().is_continue());
        let stats = c.stats();
        assert_eq!(
            (stats.divergences, stats.compared, stats.resync_events),
            (0, 2, 0)
        );
    }

    #[test]
    fn our_side_looping_more_resyncs_by_skipping_our_lines() {
        // The reference has left the loop; ours spins two more iterations.
        let reference = format!("{}\n", tl(0x8003, "inc $20", 0x0080));
        let mut c = make_cmp(&reference, 8);
        assert!(c
            .feed(&tl(0x8000, "lda $4210", 0x0042))
            .unwrap()
            .is_continue());
        assert!(c
            .feed(&tl(0x8002, "bpl $8000", 0x0042))
            .unwrap()
            .is_continue());
        assert!(c
            .feed(&tl(0x8003, "inc $20", 0x0080))
            .unwrap()
            .is_continue());
        let stats = c.stats();
        assert_eq!((stats.divergences, stats.resync_events), (0, 1));
        assert_eq!((stats.ours_skipped, stats.ref_skipped), (2, 0));
        let report = String::from_utf8(c.output().clone()).unwrap();
        assert_eq!(
            report,
            "  [resync] @ instr 0 pc 008003: skipped 2 ours / 0 ref line(s)\n"
        );
    }

    #[test]
    fn the_reference_looping_more_resyncs_by_skipping_its_lines() {
        let reference = format!(
            "{}\n{}\n{}\n",
            tl(0x8000, "lda $4210", 0x0042),
            tl(0x8002, "bpl $8000", 0x0042),
            tl(0x8003, "inc $20", 0x0080)
        );
        let mut c = make_cmp(&reference, 8);
        assert!(c
            .feed(&tl(0x8003, "inc $20", 0x0080))
            .unwrap()
            .is_continue());
        let stats = c.stats();
        assert_eq!((stats.divergences, stats.resync_events), (0, 1));
        assert_eq!((stats.ref_skipped, stats.ours_skipped), (2, 0));
    }

    #[test]
    fn a_real_divergence_is_reported_once_the_window_is_spent() {
        let reference = format!(
            "{}\n{}\n{}\n{}\n{}\n",
            tl(0x8003, "inc $20", 0x0080),
            tl(0x8004, "nop", 0),
            tl(0x8005, "nop", 0),
            tl(0x8006, "nop", 0),
            tl(0x8007, "nop", 0)
        );
        let mut c = make_cmp(&reference, 3);
        for line in [
            tl(0x9000, "jmp $9000", 0x00ff),
            tl(0x9003, "nop", 0),
            tl(0x9006, "nop", 0),
        ] {
            assert!(c.feed(&line).unwrap().is_continue());
        }
        assert!(c.feed(&tl(0x9009, "nop", 0)).unwrap().is_break());
        let stats = c.stats();
        assert_eq!(stats.divergences, 1);
        // The held divergence is the first mismatch, where the paths split.
        assert_eq!(stats.first_divergence, Some(1));
    }

    #[test]
    fn with_no_window_the_first_mismatch_is_reported_at_once() {
        let reference = format!("{}\n", tl(0x8003, "inc $20", 0x0080));
        let mut c = make_cmp(&reference, 0);
        assert!(c.feed(&tl(0x9000, "jmp $9000", 0x00ff)).unwrap().is_break());
        let stats = c.stats();
        assert_eq!((stats.divergences, stats.resync_events), (1, 0));
    }
}
