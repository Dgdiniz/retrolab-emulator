//! Genesis-specific ROM header parsers

use super::detection::*;
use retro_core::cartridge::detection::*;
use retro_core::cartridge::*;

/// The largest SRAM a cartridge header can ask for: 4 MB, far beyond any
/// real cartridge, so a header that was never filled in (a homebrew, a
/// prototype, a hack) cannot make the emulator allocate gigabytes of SRAM.
/// A larger range is cut to this size from its start.
pub const SRAM_SIZE_LIMIT: u32 = 4 * 1024 * 1024;

// ============================================================================
// Helpers
// ============================================================================

/// The size of the inclusive SRAM range `start..=end` (with `end >= start`)
/// a header gives, cut to [`SRAM_SIZE_LIMIT`]. Counted in 64 bits: a range
/// over all 32 bits is one more byte than a `u32` holds.
fn sram_range_size(start: u32, end: u32) -> usize {
    let size = u64::from(end) - u64::from(start) + 1;
    size.min(u64::from(SRAM_SIZE_LIMIT)) as usize
}

/// Extract a clean ASCII title from a Genesis header field (48 bytes).
/// Returns `None` if the field is empty or contains mostly non-printable bytes.
fn clean_header_title(raw: &[u8]) -> Option<String> {
    // Strip trailing nulls, spaces, and 0xFF padding
    let trimmed: Vec<u8> = raw
        .iter()
        .copied()
        .rev()
        .skip_while(|&b| b == 0 || b == b' ' || b == 0xFF)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    if trimmed.is_empty() {
        return None;
    }

    // Count printable ASCII characters (0x20..=0x7E)
    let printable = trimmed
        .iter()
        .filter(|&&b| (0x20..=0x7E).contains(&b))
        .count();
    let ratio = printable as f64 / trimmed.len() as f64;

    // Reject if less than 70% printable, likely binary data
    if ratio < 0.7 {
        return None;
    }

    // Replace any remaining non-printable bytes with space, then trim
    let cleaned: String = trimmed
        .iter()
        .map(|&b| {
            if (0x20..=0x7E).contains(&b) {
                b as char
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

// ============================================================================
// Genesis Header Parser
// ============================================================================

/// Genesis/Mega Drive header parser
pub struct GenesisHeaderParser;

impl GenesisHeaderParser {
    /// Create new Genesis header parser
    pub fn new() -> Self {
        GenesisHeaderParser
    }
}

impl Default for GenesisHeaderParser {
    fn default() -> Self {
        Self::new()
    }
}

impl HeaderParser for GenesisHeaderParser {
    fn console_system(&self) -> ConsoleSystem {
        ConsoleSystem::Genesis
    }

    fn validate(&self, rom_data: &[u8]) -> bool {
        if rom_data.len() < 0x200 {
            return false;
        }

        let console_name = String::from_utf8_lossy(&rom_data[0x100..0x110]);
        console_name.contains("SEGA")
    }

    fn parse(&self, rom_data: &[u8]) -> Result<HeaderInfo, String> {
        if rom_data.len() < 0x200 {
            return Err("ROM too small for header".to_string());
        }

        let header = &rom_data[0x100..0x200];

        // Parse console name
        let console_name = String::from_utf8_lossy(&header[0x00..0x10]);
        if !console_name.contains("SEGA") {
            return Err("Not a Genesis ROM (invalid console name)".to_string());
        }

        // Parse title, try domestic name first, fall back to overseas name
        let title = clean_header_title(&header[0x20..0x50])
            .or_else(|| clean_header_title(&header[0x50..0x80]))
            .unwrap_or_default();

        // Parse serial (13 bytes from 0x83)
        let serial_str = String::from_utf8_lossy(&header[0x83..0x90])
            .trim()
            .to_string();
        let serial = if !serial_str.is_empty() && !serial_str.chars().all(|c| c == '\0' || c == ' ')
        {
            Some(serial_str)
        } else {
            None
        };

        // Parse regions
        let region_str = String::from_utf8_lossy(&header[0xF0..0xF3]);
        let regions = parse_genesis_regions(&region_str);

        // Parse ROM range
        let rom_start =
            u32::from_be_bytes([header[0xA0], header[0xA1], header[0xA2], header[0xA3]]);
        let rom_end = u32::from_be_bytes([header[0xA4], header[0xA5], header[0xA6], header[0xA7]]);
        let rom_size = if rom_end >= rom_start {
            // In 64 bits, as for the SRAM range: a header can name all of them.
            usize::try_from(u64::from(rom_end) - u64::from(rom_start) + 1).unwrap_or(usize::MAX)
        } else {
            rom_data.len()
        };

        // Parse RAM range (two possible formats in the Genesis header)
        let ram_info = header[0xA8];
        let has_ram = (ram_info & 0x40) != 0;

        let (sram_size, sram_type) = if has_ram {
            // Format 1: RAM info at header[0xA8] (ROM offset 0x1A8)
            let ram_start = u32::from_be_bytes([0, header[0xA9], header[0xAA], header[0xAB]]);
            let ram_end =
                u32::from_be_bytes([header[0xAC], header[0xAD], header[0xAE], header[0xAF]]);

            if ram_end >= ram_start {
                let size = sram_range_size(ram_start, ram_end);

                let sram_type = if title.contains("SONIC") && title.contains("3") {
                    SRAMType::EEPROM
                } else {
                    SRAMType::Battery
                };

                (size, sram_type)
            } else {
                (0, SRAMType::None)
            }
        } else if header[0xB0] == b'R' && header[0xB1] == b'A' {
            // Format 2: "RA" marker at header[0xB0] (ROM offset 0x1B0)
            // Many Genesis games (e.g. Landstalker) use this format instead.
            // 0xB2: type byte (bit 7 = RAM present, bit 3 = battery backup)
            // 0xB4-0xB7: SRAM start address
            // 0xB8-0xBB: SRAM end address
            let ram_start =
                u32::from_be_bytes([header[0xB4], header[0xB5], header[0xB6], header[0xB7]]);
            let ram_end =
                u32::from_be_bytes([header[0xB8], header[0xB9], header[0xBA], header[0xBB]]);

            if ram_end >= ram_start {
                let size = sram_range_size(ram_start, ram_end);
                let sram_type = if title.contains("SONIC") && title.contains("3") {
                    SRAMType::EEPROM
                } else {
                    SRAMType::Battery
                };

                (size, sram_type)
            } else {
                (0, SRAMType::None)
            }
        } else {
            (0, SRAMType::None)
        };

        // Checksum
        let checksum = u16::from_be_bytes([header[0x8E], header[0x8F]]);

        // Parse I/O support
        let io_str = String::from_utf8_lossy(&header[0x90..0xA0])
            .trim()
            .to_string();
        let io_support: Vec<String> = io_str
            .chars()
            .filter(|&c| c != ' ' && c != '\0')
            .map(|c| c.to_string())
            .collect();

        // Determine mapper hint based on ROM size
        let mapper_hint = if rom_size > 0x400000 {
            Some(MapperId::GenesisSEGA)
        } else {
            None
        };

        // Check for expansion chip hints.
        //
        // The canonical SVP marker is the "SV" peripheral tag at ROM offset
        // 0x1C8 (the tag Genesis Plus GX detects it by). The retail Virtua
        // Racing dump carries a Shift-JIS *domestic* title and a mixed-case
        // "Virtua Racing" *overseas* title, so an exact upper-case match of the
        // title would never fire on the real ROM; the "SV" tag is what makes
        // detection robust. The title is also checked (case-insensitively) as
        // a fallback for homebrew / test images that set the field but omit
        // the tag.
        let has_sv_tag = header[0xC8] == b'S' && header[0xC9] == b'V';
        let expansion_chip_hint =
            if has_sv_tag || title.to_ascii_uppercase().contains("VIRTUA RACING") {
                Some(ExpansionChipType::SVP)
            } else {
                None
            };

        Ok(HeaderInfo {
            title,
            serial,
            regions,
            rom_size,
            sram_size,
            sram_type,
            checksum: Some(checksum),
            mapper_hint,
            expansion_chip_hint,
            io_support,
        })
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_genesis_rom(size: usize) -> Vec<u8> {
        let mut rom = vec![0; size.max(0x200)];

        // Add Genesis header at 0x100
        rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");
        rom[0x110..0x120].copy_from_slice(b"(C)TEST 2025.JAN");

        // Domestic title (48 bytes)
        let title = format!("{:<48}", "TEST ROM");
        rom[0x120..0x150].copy_from_slice(title.as_bytes());

        // Overseas title (48 bytes)
        rom[0x150..0x180].copy_from_slice(title.as_bytes());

        // Serial (13 bytes at offset 0x183)
        let serial_str = b"GM T-00000-00";
        rom[0x183..0x183 + serial_str.len()].copy_from_slice(serial_str);

        // Regions
        rom[0x1F0..0x1F3].copy_from_slice(b"JUE");

        // ROM range
        rom[0x1A0..0x1A4].copy_from_slice(&0x00000000u32.to_be_bytes());
        rom[0x1A4..0x1A8].copy_from_slice(&((size - 1) as u32).to_be_bytes());

        // No SRAM
        rom[0x1A8..0x1AC].copy_from_slice(&[0, 0, 0, 0]);
        rom[0x1AC..0x1B0].copy_from_slice(&[0, 0, 0, 0]);

        // I/O support (16 bytes)
        rom[0x190..0x1A0].copy_from_slice(b"J               ");

        rom
    }

    #[test]
    fn test_genesis_parser_validate() {
        let rom = create_test_genesis_rom(0x80000);
        let parser = GenesisHeaderParser::new();

        assert!(parser.validate(&rom));
    }

    #[test]
    fn test_genesis_parser_parse() {
        let rom = create_test_genesis_rom(0x80000);
        let parser = GenesisHeaderParser::new();

        let info = parser.parse(&rom).unwrap();

        assert_eq!(info.title, "TEST ROM");
        assert_eq!(info.serial, Some("GM T-00000-00".to_string()));
        assert_eq!(info.regions.len(), 3);
        assert!(info.regions.contains(&Region::Japan));
        assert!(info.regions.contains(&Region::USA));
        assert!(info.regions.contains(&Region::Europe));
        assert_eq!(info.rom_size, 0x80000);
        assert_eq!(info.sram_size, 0);
    }

    #[test]
    fn test_genesis_parser_with_sram() {
        let mut rom = create_test_genesis_rom(0x80000);

        // Add SRAM info to header
        // Typical Genesis SRAM: 0x200001 to 0x20FFFF (odd-byte addressing)
        // This gives us 64KB of addressable range
        rom[0x1A8] = 0xE0; // RAM indicator
        rom[0x1A9..0x1AC].copy_from_slice(&0x200001u32.to_be_bytes()[1..]);
        rom[0x1AC..0x1B0].copy_from_slice(&0x20FFFFu32.to_be_bytes());

        let parser = GenesisHeaderParser::new();
        let info = parser.parse(&rom).unwrap();

        // SRAM size is the inclusive addressable range, i.e.
        // (end - start + 1) = 0x20FFFF - 0x200001 + 1 = 0xFFFF (65535).
        assert_eq!(info.sram_size, 0xFFFF); // ~64KB
        assert_eq!(info.sram_type, SRAMType::Battery);
    }

    #[test]
    fn a_header_sram_range_past_the_limit_is_cut_to_it() {
        let mut rom = create_test_genesis_rom(0x80000);
        // "RA" SRAM from $200001 to $9FFFFF: 8 MB.
        rom[0x1B0..0x1B2].copy_from_slice(b"RA");
        rom[0x1B4..0x1B8].copy_from_slice(&0x200001u32.to_be_bytes());
        rom[0x1B8..0x1BC].copy_from_slice(&0x9FFFFFu32.to_be_bytes());
        let info = GenesisHeaderParser::new().parse(&rom).unwrap();
        assert_eq!(info.sram_size, SRAM_SIZE_LIMIT as usize);
    }

    #[test]
    fn a_header_range_over_all_32_bits_does_not_overflow() {
        let mut rom = create_test_genesis_rom(0x80000);
        rom[0x1A0..0x1A4].copy_from_slice(&0u32.to_be_bytes());
        rom[0x1A4..0x1A8].copy_from_slice(&u32::MAX.to_be_bytes());
        rom[0x1B0..0x1B2].copy_from_slice(b"RA");
        rom[0x1B4..0x1B8].copy_from_slice(&0u32.to_be_bytes());
        rom[0x1B8..0x1BC].copy_from_slice(&u32::MAX.to_be_bytes());
        let info = GenesisHeaderParser::new().parse(&rom).unwrap();
        assert_eq!(info.sram_size, SRAM_SIZE_LIMIT as usize);
        assert!(info.rom_size >= u32::MAX as usize);
    }

    #[test]
    fn test_genesis_parser_large_rom() {
        let rom = create_test_genesis_rom(0x500000); // 5MB
        let parser = GenesisHeaderParser::new();

        let info = parser.parse(&rom).unwrap();

        // Should hint SEGA mapper for large ROMs
        assert_eq!(info.mapper_hint, Some(MapperId::GenesisSEGA));
    }

    #[test]
    fn test_genesis_parser_invalid_header() {
        let mut rom = vec![0; 0x200];
        rom[0x100..0x110].copy_from_slice(b"NOT A GENESIS ! ");

        let parser = GenesisHeaderParser::new();
        assert!(!parser.validate(&rom));

        let result = parser.parse(&rom);
        assert!(result.is_err());
    }

    #[test]
    fn test_genesis_parser_virtua_racing() {
        let mut rom = create_test_genesis_rom(0x200000);

        // Update title to Virtua Racing
        let title = format!("{:<48}", "VIRTUA RACING");
        rom[0x120..0x150].copy_from_slice(title.as_bytes());

        let parser = GenesisHeaderParser::new();
        let info = parser.parse(&rom).unwrap();

        assert_eq!(info.expansion_chip_hint, Some(ExpansionChipType::SVP));
    }

    #[test]
    fn test_genesis_parser_svp_sv_tag() {
        // The retail Virtua Racing dump has no upper-case "VIRTUA RACING"
        // title (domestic is Shift-JIS, overseas is mixed case), so detection
        // must key off the "SV" peripheral tag at ROM 0x1C8.
        let mut rom = create_test_genesis_rom(0x200000);
        // A non-matching title, so only the tag can trigger SVP detection.
        let title = format!("{:<48}", "SOME RACER");
        rom[0x120..0x150].copy_from_slice(title.as_bytes());
        rom[0x150..0x180].copy_from_slice(title.as_bytes());
        // "SV" tag.
        rom[0x1C8] = b'S';
        rom[0x1C9] = b'V';

        let parser = GenesisHeaderParser::new();
        let info = parser.parse(&rom).unwrap();
        assert_eq!(info.expansion_chip_hint, Some(ExpansionChipType::SVP));

        // Without the tag (and no title hint) it is a plain cartridge.
        rom[0x1C8] = 0;
        rom[0x1C9] = 0;
        let info = parser.parse(&rom).unwrap();
        assert_eq!(info.expansion_chip_hint, None);
    }

    #[test]
    fn test_genesis_parser_sonic_3() {
        let mut rom = create_test_genesis_rom(0x400000);

        // Update title to Sonic 3
        let title = format!("{:<48}", "SONIC 3");
        rom[0x120..0x150].copy_from_slice(title.as_bytes());

        // Add SRAM
        rom[0x1A8] = 0xE0;
        rom[0x1A9..0x1AC].copy_from_slice(&0x200001u32.to_be_bytes()[1..]);
        rom[0x1AC..0x1B0].copy_from_slice(&0x20FFFFu32.to_be_bytes());

        let parser = GenesisHeaderParser::new();
        let info = parser.parse(&rom).unwrap();

        assert_eq!(info.sram_type, SRAMType::EEPROM);
    }

    #[test]
    fn test_clean_header_title_basic() {
        let mut buf = [0x20u8; 48];
        buf[..8].copy_from_slice(b"GAIARES ");
        assert_eq!(clean_header_title(&buf), Some("GAIARES".to_string()));
    }

    #[test]
    fn test_clean_header_title_binary_garbage() {
        // Fill with mostly non-printable bytes, should return None
        let buf = [0xFF; 48];
        assert_eq!(clean_header_title(&buf), None);
    }

    #[test]
    fn test_clean_header_title_mixed_junk() {
        // Some printable mixed with binary, below 70% threshold
        let mut buf = [0x80u8; 48];
        buf[0] = b'A';
        buf[1] = b'B';
        assert_eq!(clean_header_title(&buf), None);
    }

    #[test]
    fn test_genesis_parser_binary_domestic_falls_back_to_overseas() {
        let mut rom = create_test_genesis_rom(0x80000);
        // Fill domestic title with binary garbage
        rom[0x120..0x150].fill(0x80);
        // Put valid overseas title
        let overseas = format!("{:<48}", "GAIARES OVERSEAS");
        rom[0x150..0x180].copy_from_slice(overseas.as_bytes());

        let parser = GenesisHeaderParser::new();
        let info = parser.parse(&rom).unwrap();
        assert_eq!(info.title, "GAIARES OVERSEAS");
    }
}
