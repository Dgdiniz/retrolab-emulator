//! The parsers of the values the commands take on their command lines.

use std::collections::BTreeSet;

/// The digits of a hexadecimal number, its `0x`, `0X` or `$` prefix and the
/// spaces around it taken off.
fn hex_digits(s: &str) -> &str {
    s.trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X")
        .trim_start_matches('$')
}

/// A hexadecimal number, with or without a `0x`, `0X` or `$` prefix
/// (`ff0000`, `0xFF0000`, `$FF0000`), as clap's value parser for an address
/// or a length.
pub fn parse_hex_u32(s: &str) -> Result<u32, String> {
    u32::from_str_radix(hex_digits(s), 16).map_err(|e| format!("invalid hex number '{s}': {e}"))
}

/// A hexadecimal byte, with or without a prefix (`a9`, `0xA9`, `$A9`), as
/// clap's value parser for an opcode.
pub fn parse_hex_u8(s: &str) -> Result<u8, String> {
    u8::from_str_radix(hex_digits(s), 16).map_err(|e| format!("invalid hex number '{s}': {e}"))
}

/// An inclusive range of addresses `bits` wide, `START-END` or a single
/// address, each side hexadecimal with or without a prefix (`C000-CFFF`,
/// `0x0000-0xFFFF`, `$D6EBA`).
pub fn parse_hex_range(spec: &str, bits: u8) -> Result<(u32, u32), String> {
    let address = |s: &str| -> Result<u32, String> {
        let value = parse_hex_u32(s)?;
        if bits < 32 && value >> bits != 0 {
            return Err(format!(
                "address '{}' in range '{spec}' is wider than {bits} bits",
                s.trim()
            ));
        }
        Ok(value)
    };
    let (start, end) = match spec.split_once('-') {
        Some((start, end)) => (address(start)?, address(end)?),
        None => {
            let at = address(spec)?;
            (at, at)
        }
    };
    if start > end {
        return Err(format!("range '{spec}' ends before it starts"));
    }
    Ok((start, end))
}

/// A comma-separated list of frame numbers (`60,300,600`), as a set; empty
/// for an empty list.
pub fn parse_frame_list(spec: &str) -> Result<BTreeSet<u64>, String> {
    spec.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<u64>()
                .map_err(|_| format!("invalid frame {s:?} (want an integer)"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hex_number_takes_each_prefix() {
        assert_eq!(parse_hex_u32("ff0000"), Ok(0xFF_0000));
        assert_eq!(parse_hex_u32("0x00FFC0"), Ok(0x00_FFC0));
        assert_eq!(parse_hex_u32("0X00FFC0"), Ok(0x00_FFC0));
        assert_eq!(parse_hex_u32("$1"), Ok(1));
        assert_eq!(parse_hex_u32(" 7E0000 "), Ok(0x7E_0000));
        assert_eq!(parse_hex_u8("a9"), Ok(0xA9));
        assert_eq!(parse_hex_u8("0xE8"), Ok(0xE8));
        assert_eq!(parse_hex_u8("$1b"), Ok(0x1B));
        assert_eq!(parse_hex_u8("  3C "), Ok(0x3C));
    }

    #[test]
    fn a_word_that_is_not_hex_names_itself_in_the_error() {
        let err = parse_hex_u32("nope").unwrap_err();
        assert_eq!(
            err,
            "invalid hex number 'nope': invalid digit found in string"
        );
        assert!(parse_hex_u32("").is_err());
        assert!(parse_hex_u32("100000000").is_err());
        assert!(parse_hex_u8("zz").is_err());
        assert!(parse_hex_u8("100").is_err());
        assert!(parse_hex_u8("").is_err());
    }

    #[test]
    fn a_range_is_two_addresses_or_one_within_its_width() {
        assert_eq!(parse_hex_range("C000-CFFF", 16), Ok((0xC000, 0xCFFF)));
        assert_eq!(parse_hex_range("0x0000-0xFFFF", 16), Ok((0, 0xFFFF)));
        assert_eq!(parse_hex_range("$D6EBA", 24), Ok((0xD_6EBA, 0xD_6EBA)));
        assert_eq!(parse_hex_range("D6EBA-DC0BF", 24), Ok((0xD_6EBA, 0xD_C0BF)));
        assert_eq!(
            parse_hex_range("10000", 16).unwrap_err(),
            "address '10000' in range '10000' is wider than 16 bits"
        );
        assert_eq!(
            parse_hex_range("200-100", 24).unwrap_err(),
            "range '200-100' ends before it starts"
        );
        assert!(parse_hex_range("ZZZZ-0000", 16).is_err());
    }

    #[test]
    fn a_frame_list_is_a_set_of_integers() {
        assert_eq!(parse_frame_list(""), Ok(BTreeSet::new()));
        assert_eq!(
            parse_frame_list(" 60, 300 ,60"),
            Ok(BTreeSet::from([60, 300]))
        );
        assert_eq!(
            parse_frame_list("60,x").unwrap_err(),
            "invalid frame \"x\" (want an integer)"
        );
    }
}
