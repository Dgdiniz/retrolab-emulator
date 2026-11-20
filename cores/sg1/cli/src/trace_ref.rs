//! The parsers of MAME-format Mega Drive trace lines, ours and MAME's, into
//! the [`TraceLine`] the shared comparison reads.
//!
//! A 68000 line is `PC:.. SR:.. D0:.. .. A7:.. | disassembly`, with MAME's
//! timing columns (`TC`/`FR`/`BX`/`BY`) in front of it when it carries them
//! (our trace writes `TC` and `BY` with `--timing`, and in `compare-trace`).
//! The disassembly takes no part: MAME's and ours spell instructions
//! differently. A Z80 line is `PC:.. AF:.. .. HALT:.. | disassembly`, with
//! the same timing columns in MAME's.

use retro_cli::trace::line::{
    shown_decimal, shown_hex_upper, FieldValue, Show, TraceField, TraceLine,
};

const DATA_REGISTERS: [&str; 8] = ["D0", "D1", "D2", "D3", "D4", "D5", "D6", "D7"];
const ADDRESS_REGISTERS: [&str; 8] = ["A0", "A1", "A2", "A3", "A4", "A5", "A6", "A7"];

/// The timing columns, as MAME writes them and in its order.
const TIMING: [(&str, Show); 4] = [
    ("TC", shown_hex_upper::<16>),
    ("FR", shown_hex_upper::<6>),
    ("BX", shown_hex_upper::<4>),
    ("BY", shown_hex_upper::<4>),
];

/// Parse a 68000 line, or `None` for a line with no `PC` (a blank line, an
/// interrupt note). A register the line lacks reads 0; a timing column it
/// lacks takes no part. The program counter is the 68000's 24 bits.
pub fn parse_m68k(line: &str) -> Option<TraceLine> {
    let registers = line.split('|').next()?;
    let mut pc = None;
    let mut sr = 0u16;
    let mut d = [0u32; 8];
    let mut a = [0u32; 8];
    let mut timing = [None; 4];

    for part in registers.split_whitespace() {
        let Some((name, value)) = part.split_once(':') else {
            continue;
        };
        if let Some(column) = TIMING.iter().position(|(column, _)| *column == name) {
            timing[column] = u64::from_str_radix(value, 16).ok();
            continue;
        }
        let Ok(value) = u32::from_str_radix(value, 16) else {
            continue;
        };
        match name {
            "PC" => pc = Some(value),
            "SR" => sr = value as u16,
            _ => {
                if let Some(i) = DATA_REGISTERS.iter().position(|r| *r == name) {
                    d[i] = value;
                } else if let Some(i) = ADDRESS_REGISTERS.iter().position(|r| *r == name) {
                    a[i] = value;
                }
            }
        }
    }

    let pc = pc? & 0x00FF_FFFF;
    let mut fields = Vec::with_capacity(22);
    fields.push(TraceField::number("PC", pc as u64, shown_hex_upper::<6>));
    fields.push(TraceField::number("SR", sr as u64, shown_sr));
    for (&name, value) in DATA_REGISTERS.iter().zip(d) {
        fields.push(TraceField::number(name, value as u64, shown_hex_upper::<8>));
    }
    for (&name, value) in ADDRESS_REGISTERS.iter().zip(a) {
        fields.push(TraceField::number(name, value as u64, shown_hex_upper::<8>));
    }
    push_timing(&mut fields, timing);
    Some(TraceLine { pc, fields })
}

/// Parse a Z80 line, or `None` for a line with no `PC` or with a register
/// that does not read.
pub fn parse_z80(line: &str) -> Option<TraceLine> {
    const PAIRS: [&str; 8] = ["PC", "AF", "BC", "DE", "HL", "IX", "IY", "SP"];
    const BYTES: [&str; 2] = ["I", "R"];
    const BITS: [&str; 3] = ["IFF1", "IFF2", "HALT"];

    let registers = line.split('|').next()?;
    let mut pairs = [0u64; 8];
    let mut bytes = [0u64; 2];
    let mut im = 0u64;
    let mut bits = [0u64; 3];
    let mut timing = [None; 4];
    let mut found_pc = false;

    for part in registers.split_whitespace() {
        let Some((name, value)) = part.split_once(':') else {
            continue;
        };
        if let Some(column) = TIMING.iter().position(|(column, _)| *column == name) {
            timing[column] = u64::from_str_radix(value, 16).ok();
        } else if let Some(i) = PAIRS.iter().position(|r| *r == name) {
            pairs[i] = u16::from_str_radix(value, 16).ok()? as u64;
            found_pc |= i == 0;
        } else if let Some(i) = BYTES.iter().position(|r| *r == name) {
            bytes[i] = u8::from_str_radix(value, 16).ok()? as u64;
        } else if name == "IM" {
            im = value.parse::<u8>().ok()? as u64;
        } else if let Some(i) = BITS.iter().position(|r| *r == name) {
            bits[i] = u64::from(value == "1");
        }
    }
    if !found_pc {
        return None;
    }

    let mut fields = Vec::with_capacity(18);
    for (&name, value) in PAIRS.iter().zip(pairs) {
        fields.push(TraceField::number(name, value, shown_hex_upper::<4>));
    }
    for (&name, value) in BYTES.iter().zip(bytes) {
        fields.push(TraceField::number(name, value, shown_hex_upper::<2>));
    }
    fields.push(TraceField::number("IM", im, shown_decimal));
    for (&name, value) in BITS.iter().zip(bits) {
        fields.push(TraceField::number(name, value, shown_decimal));
    }
    push_timing(&mut fields, timing);
    Some(TraceLine {
        pc: pairs[0] as u32,
        fields,
    })
}

/// The timing columns, after the registers: a column the line lacks takes
/// no part.
fn push_timing(fields: &mut Vec<TraceField>, timing: [Option<u64>; 4]) {
    for (&(name, show), value) in TIMING.iter().zip(timing) {
        let value = value.map_or(FieldValue::Missing, FieldValue::Number);
        fields.push(TraceField::of(name, value, show).as_timing());
    }
}

/// The status register with its condition codes: `2704 (..Z..)`.
fn shown_sr(value: &FieldValue) -> String {
    match value {
        FieldValue::Number(sr) => format!("{sr:04X} ({})", format_sr_flags(*sr as u16)),
        other => shown_hex_upper::<4>(other),
    }
}

/// The 68000's condition codes as `XNZVC`, a clear bit shown as `.`.
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

#[cfg(test)]
mod tests {
    use super::*;
    use retro_cli::trace::compare::{Comparator, CompareRules};
    use std::collections::HashSet;
    use std::io::Cursor;
    use std::ops::ControlFlow;

    fn number(line: &TraceLine, name: &str) -> FieldValue {
        line.field(name).expect("field").value.clone()
    }

    const MAME_LINE: &str = "TC:0000000000000022 FR:000000 BX:0054 BY:00E0 PC:000200 SR:2700 \
        D0:00000001 D1:00000000 D2:00000000 D3:00000000 D4:00000000 D5:00000000 D6:00000000 \
        D7:00000000 A0:00000000 A1:00000000 A2:00000000 A3:00000000 A4:00000000 A5:00000000 \
        A6:00000000 A7:FFFFFF00 | 000200: tst.l $a10008.l";

    #[test]
    fn a_mame_68000_line_reads_with_its_timing() {
        let e = parse_m68k(MAME_LINE).expect("parses");
        assert_eq!(e.pc, 0x000200);
        assert_eq!(number(&e, "SR"), FieldValue::Number(0x2700));
        assert_eq!(number(&e, "D0"), FieldValue::Number(1));
        assert_eq!(number(&e, "A7"), FieldValue::Number(0xFFFF_FF00));
        assert_eq!(number(&e, "TC"), FieldValue::Number(0x22));
        assert_eq!(number(&e, "BY"), FieldValue::Number(0xE0));
        assert!(e.field("TC").unwrap().timing);
    }

    #[test]
    fn a_bare_68000_line_reads_without_timing() {
        let line = "PC:000206 SR:2708 D0:00000000 D1:00000000 D2:00000000 D3:00000000 \
            D4:00000000 D5:00000000 D6:00000000 D7:00000000 A0:00000000 A1:00000000 \
            A2:00000000 A3:00000000 A4:00000000 A5:00000000 A6:00000000 A7:00FFFE00 | TST.L $00A10008";
        let e = parse_m68k(line).expect("parses");
        assert_eq!(e.pc, 0x000206);
        assert_eq!(number(&e, "TC"), FieldValue::Missing);
    }

    #[test]
    fn a_line_with_no_pc_does_not_parse() {
        assert!(parse_m68k("").is_none());
        assert!(parse_m68k("(interrupted at 000932, IRQ 6)").is_none());
        assert!(parse_z80("").is_none());
    }

    #[test]
    fn the_68000_pc_is_compared_on_24_bits() {
        let a = parse_m68k("PC:FF000200 SR:0000").unwrap();
        let b = parse_m68k("PC:00000200 SR:0000").unwrap();
        assert!(!a.differs_from(&b, false, &HashSet::new()));
        assert_eq!(a.pc_text(), "000200");
    }

    #[test]
    fn a_differing_register_is_reported_as_the_report_always_showed_it() {
        let a = parse_m68k("PC:000200 SR:2704 D0:00000001").unwrap();
        let b = parse_m68k("PC:000200 SR:2700 D0:00000002").unwrap();
        let diffs = a.differences(&b, false, &HashSet::new());
        let shown: Vec<(&str, &str, &str)> = diffs
            .iter()
            .map(|d| (d.field, d.ours.as_str(), d.theirs.as_str()))
            .collect();
        assert_eq!(
            shown,
            vec![
                ("SR", "2704 (..Z..)", "2700 (.....)"),
                ("D0", "00000001", "00000002")
            ]
        );
    }

    #[test]
    fn an_ignored_register_takes_no_part() {
        let a = parse_m68k("PC:000200 A7:00000100").unwrap();
        let b = parse_m68k("PC:000200 A7:00000200").unwrap();
        let ignore: HashSet<String> = ["A7".to_string()].into();
        assert!(!a.differs_from(&b, false, &ignore));
    }

    #[test]
    fn the_timing_columns_both_lines_carry_take_part_unless_skipped() {
        let ours = parse_m68k("TC:0000000000000010 BY:0000 PC:000400 SR:0000").unwrap();
        let theirs =
            parse_m68k("TC:0000000000000010 FR:000000 BX:0004 BY:00E0 PC:000400 SR:0000").unwrap();
        let diffs = ours.differences(&theirs, false, &HashSet::new());
        assert_eq!(diffs.len(), 1);
        assert_eq!(
            (
                diffs[0].field,
                diffs[0].ours.as_str(),
                diffs[0].theirs.as_str()
            ),
            ("BY", "0000", "00E0")
        );
        assert!(!ours.differs_from(&theirs, true, &HashSet::new()));
    }

    #[test]
    fn a_mame_z80_line_reads_with_its_timing() {
        let line = "TC:0000000000001000 FR:000001 BX:0000 BY:0000 PC:0008 AF:0040 BC:0000 \
            DE:0000 HL:0000 IX:0000 IY:0000 SP:FFFE I:00 R:02 IM:1 IFF1:1 IFF2:1 HALT:0 | ei";
        let e = parse_z80(line).expect("parses");
        assert_eq!(e.pc, 0x0008);
        assert_eq!(number(&e, "AF"), FieldValue::Number(0x0040));
        assert_eq!(number(&e, "R"), FieldValue::Number(0x02));
        assert_eq!(number(&e, "IM"), FieldValue::Number(1));
        assert_eq!(number(&e, "IFF1"), FieldValue::Number(1));
        assert_eq!(number(&e, "HALT"), FieldValue::Number(0));
        assert_eq!(number(&e, "TC"), FieldValue::Number(0x1000));
    }

    #[test]
    fn a_z80_flag_difference_is_one_field() {
        let a = parse_z80("PC:0010 IFF1:1").unwrap();
        let b = parse_z80("PC:0010 IFF1:0").unwrap();
        let diffs = a.differences(&b, false, &HashSet::new());
        assert_eq!(diffs.len(), 1);
        assert_eq!((diffs[0].field, diffs[0].ours.as_str()), ("IFF1", "1"));
    }

    #[test]
    fn the_condition_codes_read_as_letters() {
        assert_eq!(format_sr_flags(0x0000), ".....");
        assert_eq!(format_sr_flags(0x001F), "XNZVC");
        assert_eq!(format_sr_flags(0x0008), ".N...");
    }

    // The shared comparator on MAME's lines.

    /// A 68000 line; `d0` differs per loop iteration so that two iterations
    /// never match during a resync.
    fn tl(pc: u32, disasm: &str, d0: u32) -> String {
        format!(
            "PC:{pc:06X} SR:0000 D0:{d0:08X} D1:00000000 D2:00000000 D3:00000000 D4:00000000 \
             D5:00000000 D6:00000000 D7:00000000 A0:00000000 A1:00000000 A2:00000000 A3:00000000 \
             A4:00000000 A5:00000000 A6:00000000 A7:00000000 | {disasm}"
        )
    }

    fn make_cmp(
        reference: &str,
        budget: usize,
        skip_timing: bool,
    ) -> Comparator<Cursor<Vec<u8>>, Vec<u8>> {
        let rules = CompareRules {
            parse: parse_m68k,
            skip_timing,
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
        let reference = format!("{}\n{}\n", tl(0x400, "NOP", 0), tl(0x402, "NOP", 0));
        let mut c = make_cmp(&reference, 0, false);
        assert!(c.feed(&tl(0x400, "NOP", 0)).unwrap().is_continue());
        assert!(c.feed(&tl(0x402, "NOP", 0)).unwrap().is_continue());
        let stats = c.stats();
        assert_eq!(
            (stats.divergences, stats.compared, stats.resync_events),
            (0, 2, 0)
        );
    }

    #[test]
    fn our_side_looping_more_resyncs_by_skipping_our_lines() {
        // The reference has left the loop; ours spins two more iterations.
        let reference = format!("{}\n", tl(0x408, "ADDQ", 0x0080));
        let mut c = make_cmp(&reference, 8, false);
        assert!(c.feed(&tl(0x400, "BTST", 0x0042)).unwrap().is_continue());
        assert!(c.feed(&tl(0x404, "BEQ", 0x0042)).unwrap().is_continue());
        assert!(c.feed(&tl(0x408, "ADDQ", 0x0080)).unwrap().is_continue());
        let stats = c.stats();
        assert_eq!((stats.divergences, stats.resync_events), (0, 1));
        assert_eq!((stats.ours_skipped, stats.ref_skipped), (2, 0));
    }

    #[test]
    fn the_reference_looping_more_resyncs_by_skipping_its_lines() {
        let reference = format!(
            "{}\n{}\n{}\n",
            tl(0x400, "BTST", 0x0042),
            tl(0x404, "BEQ", 0x0042),
            tl(0x408, "ADDQ", 0x0080)
        );
        let mut c = make_cmp(&reference, 8, false);
        assert!(c.feed(&tl(0x408, "ADDQ", 0x0080)).unwrap().is_continue());
        let stats = c.stats();
        assert_eq!((stats.divergences, stats.resync_events), (0, 1));
        assert_eq!((stats.ref_skipped, stats.ours_skipped), (2, 0));
        let report = String::from_utf8(c.output().clone()).unwrap();
        assert_eq!(
            report,
            "  [resync] @ instr 0 pc 000408: skipped 0 ours / 2 ref line(s)\n"
        );
    }

    #[test]
    fn a_real_divergence_is_reported_once_the_window_is_spent() {
        let reference = format!(
            "{}\n{}\n{}\n{}\n{}\n",
            tl(0x408, "ADDQ", 0x0080),
            tl(0x40A, "NOP", 0),
            tl(0x40C, "NOP", 0),
            tl(0x40E, "NOP", 0),
            tl(0x410, "NOP", 0)
        );
        let mut c = make_cmp(&reference, 3, false);
        for line in [
            tl(0x9000, "JMP", 0x00FF),
            tl(0x9006, "NOP", 0),
            tl(0x900C, "NOP", 0),
        ] {
            assert!(c.feed(&line).unwrap().is_continue());
        }
        assert!(c.feed(&tl(0x9012, "NOP", 0)).unwrap().is_break());
        let stats = c.stats();
        assert_eq!(stats.divergences, 1);
        // The held divergence is the first mismatch, where the paths split.
        assert_eq!(stats.first_divergence, Some(1));
    }

    #[test]
    fn with_no_window_the_first_mismatch_is_reported_at_once() {
        let reference = format!("{}\n", tl(0x408, "ADDQ", 0x0080));
        let mut c = make_cmp(&reference, 0, false);
        assert!(matches!(
            c.feed(&tl(0x9000, "JMP", 0x00FF)).unwrap(),
            ControlFlow::Break(())
        ));
        let stats = c.stats();
        assert_eq!((stats.divergences, stats.resync_events), (1, 0));
    }

    #[test]
    fn under_skip_timing_a_timing_difference_is_counted_not_reported() {
        let ours = format!("TC:0000000000000010 BY:0000 {}", tl(0x400, "NOP", 0));
        let reference = format!("TC:0000000000000099 BY:00E0 {}\n", tl(0x400, "NOP", 0));
        let mut c = make_cmp(&reference, 0, true);
        assert!(c.feed(&ours).unwrap().is_continue());
        let stats = c.stats();
        assert_eq!(
            (stats.divergences, stats.compared, stats.timing_skipped),
            (0, 1, 1)
        );

        let mut c = make_cmp(&reference, 0, false);
        assert!(c.feed(&ours).unwrap().is_break());
        assert_eq!(c.stats().divergences, 1);
    }
}
