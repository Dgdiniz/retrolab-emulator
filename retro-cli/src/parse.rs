//! The parsers of the values the commands take on their command lines.

/// A hexadecimal number, with or without a `0x` or `$` prefix (`ff0000`,
/// `0xFF0000`, `$FF0000`), as clap's value parser for an address or a
/// length.
pub fn parse_hex_u32(s: &str) -> Result<u32, String> {
    let digits = s.trim().trim_start_matches("0x").trim_start_matches('$');
    u32::from_str_radix(digits, 16).map_err(|e| format!("invalid hex number '{s}': {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hex_number_takes_each_prefix() {
        assert_eq!(parse_hex_u32("ff0000"), Ok(0xFF_0000));
        assert_eq!(parse_hex_u32("0x00FFC0"), Ok(0x00_FFC0));
        assert_eq!(parse_hex_u32("$1"), Ok(1));
        assert_eq!(parse_hex_u32(" 7E0000 "), Ok(0x7E_0000));
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
    }
}
