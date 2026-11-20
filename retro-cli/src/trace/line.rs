//! A parsed trace line, and the differ two of them are compared with.
//!
//! A console's parsers (its `trace_ref` module) read its own trace text and
//! its reference emulator's into a [`TraceLine`]: the program counter and
//! the line's fields, always the same fields in the same order for a CPU,
//! so two lines of one CPU are compared field by field. A field the line
//! does not carry is [`FieldValue::Missing`] and takes no part.

use std::collections::HashSet;

/// What a field of a trace line holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldValue {
    /// A number: a register, an address, a counter.
    Number(u64),
    /// A text: a disassembly, a set of flag letters.
    Text(String),
    /// Nothing, as the line says: an instruction with no effective address.
    Empty,
    /// The line has no such column.
    Missing,
}

/// How a field's value is written in a report.
pub type Show = fn(&FieldValue) -> String;

/// One field of a trace line.
#[derive(Clone, Debug)]
pub struct TraceField {
    /// Its name, as a report prints it and `--ignore` names it.
    pub name: &'static str,
    /// What the line holds there.
    pub value: FieldValue,
    /// Whether it is a timing column, which `--skip-timing` leaves out.
    pub timing: bool,
    /// Its text in a report, made only when a report prints it.
    pub show: Show,
}

impl TraceField {
    /// A number field.
    pub fn number(name: &'static str, value: u64, show: Show) -> Self {
        Self {
            name,
            value: FieldValue::Number(value),
            timing: false,
            show,
        }
    }

    /// A text field.
    pub fn text(name: &'static str, value: String) -> Self {
        Self {
            name,
            value: FieldValue::Text(value),
            timing: false,
            show: shown_text,
        }
    }

    /// A field of any value.
    pub fn of(name: &'static str, value: FieldValue, show: Show) -> Self {
        Self {
            name,
            value,
            timing: false,
            show,
        }
    }

    /// The same field, as a timing column.
    pub fn as_timing(mut self) -> Self {
        self.timing = true;
        self
    }

    /// Its text in a report.
    pub fn shown(&self) -> String {
        (self.show)(&self.value)
    }
}

/// A field whose values differ between two lines, for a report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldDiff {
    pub field: &'static str,
    pub ours: String,
    pub theirs: String,
}

/// A parsed trace line.
#[derive(Clone, Debug)]
pub struct TraceLine {
    /// The instruction's address.
    pub pc: u32,
    /// The line's fields, the same ones in the same order for every line of
    /// a CPU.
    pub fields: Vec<TraceField>,
}

impl TraceLine {
    /// The field of this name.
    pub fn field(&self, name: &str) -> Option<&TraceField> {
        self.fields.iter().find(|field| field.name == name)
    }

    /// The program counter as the line's `PC` field shows it.
    pub fn pc_text(&self) -> String {
        match self.field("PC") {
            Some(field) => field.shown(),
            None => format!("{:06x}", self.pc),
        }
    }

    /// Whether a field this comparison looks at differs from `other`'s: a
    /// field either line lacks takes no part, nor does a field `ignore`
    /// names (upper-cased names), nor a timing column with `skip_timing`.
    pub fn differs_from(
        &self,
        other: &TraceLine,
        skip_timing: bool,
        ignore: &HashSet<String>,
    ) -> bool {
        self.compared(other, skip_timing, ignore).next().is_some()
    }

    /// The fields this comparison looks at that differ from `other`'s, in
    /// the line's order.
    pub fn differences(
        &self,
        other: &TraceLine,
        skip_timing: bool,
        ignore: &HashSet<String>,
    ) -> Vec<FieldDiff> {
        self.compared(other, skip_timing, ignore)
            .map(|(ours, theirs)| FieldDiff {
                field: ours.name,
                ours: ours.shown(),
                theirs: theirs.shown(),
            })
            .collect()
    }

    fn compared<'a>(
        &'a self,
        other: &'a TraceLine,
        skip_timing: bool,
        ignore: &'a HashSet<String>,
    ) -> impl Iterator<Item = (&'a TraceField, &'a TraceField)> + 'a {
        self.fields
            .iter()
            .zip(&other.fields)
            .filter(move |(ours, theirs)| {
                !(skip_timing && ours.timing)
                    && ours.value != FieldValue::Missing
                    && theirs.value != FieldValue::Missing
                    && ours.value != theirs.value
                    && !is_ignored(ignore, ours.name)
            })
    }
}

/// Whether `ignore` (upper-cased names) names this field.
fn is_ignored(ignore: &HashSet<String>, name: &str) -> bool {
    ignore
        .iter()
        .any(|ignored| ignored.eq_ignore_ascii_case(name))
}

/// Parse a comma-separated `--ignore` list into an upper-cased set.
pub fn parse_ignore(spec: Option<&str>) -> HashSet<String> {
    spec.map(|s| {
        s.split(',')
            .map(|name| name.trim().to_ascii_uppercase())
            .filter(|name| !name.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

/// The hexadecimal number right after `label` in `s`, up to the first
/// character that is not a hex digit.
pub fn hex_after(s: &str, label: &str) -> Option<u64> {
    let start = s.find(label)? + label.len();
    let rest = &s[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_hexdigit())
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    u64::from_str_radix(&rest[..end], 16).ok()
}

/// The decimal number after `label` in `s`, past any spaces, with an
/// optional leading `-` (bsnes-plus pads these columns: `V:  0`).
pub fn dec_after(s: &str, label: &str) -> Option<i64> {
    let start = s.find(label)? + label.len();
    let rest = s[start..].trim_start();
    let (negative, digits) = match rest.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, rest),
    };
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    if end == 0 {
        return None;
    }
    let value: i64 = digits[..end].parse().ok()?;
    Some(if negative { -value } else { value })
}

/// A field's text with a number written by `number`; a text as it is,
/// nothing as `none`.
fn shown_with(value: &FieldValue, number: impl Fn(u64) -> String) -> String {
    match value {
        FieldValue::Number(n) => number(*n),
        FieldValue::Text(text) => text.clone(),
        FieldValue::Empty => "none".to_string(),
        FieldValue::Missing => "-".to_string(),
    }
}

/// Lower-case hexadecimal, `W` digits at least.
pub fn shown_hex<const W: usize>(value: &FieldValue) -> String {
    shown_with(value, |n| format!("{n:0W$x}"))
}

/// Upper-case hexadecimal, `W` digits at least.
pub fn shown_hex_upper<const W: usize>(value: &FieldValue) -> String {
    shown_with(value, |n| format!("{n:0W$X}"))
}

/// Decimal.
pub fn shown_decimal(value: &FieldValue) -> String {
    shown_with(value, |n| n.to_string())
}

/// Decimal of a signed number stored as its two's complement.
pub fn shown_signed(value: &FieldValue) -> String {
    shown_with(value, |n| (n as i64).to_string())
}

/// A text as it is.
pub fn shown_text(value: &FieldValue) -> String {
    shown_with(value, |n| n.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(pc: u64, a: u64, timing: Option<u64>) -> TraceLine {
        TraceLine {
            pc: pc as u32,
            fields: vec![
                TraceField::number("PC", pc, shown_hex::<6>),
                TraceField::number("A", a, shown_hex::<0>),
                TraceField::text("disasm", "nop".to_string()),
                TraceField::of(
                    "V",
                    timing.map_or(FieldValue::Missing, FieldValue::Number),
                    shown_decimal,
                )
                .as_timing(),
            ],
        }
    }

    #[test]
    fn a_differing_field_is_reported_with_both_texts() {
        let diffs =
            line(0x8000, 1, None).differences(&line(0x8000, 0xff, None), false, &HashSet::new());
        assert_eq!(
            diffs,
            vec![FieldDiff {
                field: "A",
                ours: "1".to_string(),
                theirs: "ff".to_string()
            }]
        );
    }

    #[test]
    fn a_field_either_line_lacks_takes_no_part() {
        let ours = line(0x8000, 1, None);
        let theirs = line(0x8000, 1, Some(5));
        assert!(!ours.differs_from(&theirs, false, &HashSet::new()));
        assert!(!theirs.differs_from(&ours, false, &HashSet::new()));
    }

    #[test]
    fn a_timing_column_both_lines_carry_is_compared_unless_skipped() {
        let ours = line(0x8000, 1, Some(4));
        let theirs = line(0x8000, 1, Some(5));
        let diffs = ours.differences(&theirs, false, &HashSet::new());
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].field, "V");
        assert!(!ours.differs_from(&theirs, true, &HashSet::new()));
    }

    #[test]
    fn an_ignored_field_takes_no_part_whatever_its_case() {
        let ignore = parse_ignore(Some(" a , pc,, Disasm "));
        assert_eq!(ignore.len(), 3);
        assert!(ignore.contains("A") && ignore.contains("PC") && ignore.contains("DISASM"));
        assert!(!line(0x8000, 1, None).differs_from(&line(0x8002, 2, None), false, &ignore));
        assert!(parse_ignore(None).is_empty());
    }

    #[test]
    fn the_pc_text_is_the_pc_field_as_shown() {
        assert_eq!(line(0x81_8000, 0, None).pc_text(), "818000");
        let bare = TraceLine {
            pc: 0x1234,
            fields: Vec::new(),
        };
        assert_eq!(bare.pc_text(), "001234");
    }

    #[test]
    fn the_number_readers_stop_at_the_first_other_character() {
        assert_eq!(hex_after("A:00ff X:0001", "A:"), Some(0xff));
        assert_eq!(hex_after("A: X:0001", "A:"), None);
        assert_eq!(hex_after("X:0001", "A:"), None);
        assert_eq!(dec_after("V:  12 H:-3", "V:"), Some(12));
        assert_eq!(dec_after("V:  12 H:-3", "H:"), Some(-3));
        assert_eq!(dec_after("V: x", "V:"), None);
    }

    #[test]
    fn the_texts_are_made_as_asked() {
        assert_eq!(shown_hex::<6>(&FieldValue::Number(0x1f)), "00001f");
        assert_eq!(shown_hex_upper::<4>(&FieldValue::Number(0xe0)), "00E0");
        assert_eq!(shown_hex::<0>(&FieldValue::Number(0)), "0");
        assert_eq!(shown_signed(&FieldValue::Number(-3i64 as u64)), "-3");
        assert_eq!(shown_hex::<6>(&FieldValue::Empty), "none");
        assert_eq!(
            shown_text(&FieldValue::Text("..1B.I..".to_string())),
            "..1B.I.."
        );
    }
}
