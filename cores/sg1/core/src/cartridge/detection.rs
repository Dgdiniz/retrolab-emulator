//! Genesis-specific ROM detection utilities

use retro_core::cartridge::*;

/// Calculate Genesis checksum
///
/// Genesis ROMs have a 16-bit checksum in the header that is the sum
/// of all 16-bit words from offset 0x200 onwards.
pub fn calculate_genesis_checksum(rom_data: &[u8]) -> u16 {
    let mut checksum: u32 = 0;

    // Sum all words from 0x200 onwards
    for chunk in rom_data[0x200..].chunks(2) {
        if chunk.len() == 2 {
            let word = u16::from_be_bytes([chunk[0], chunk[1]]) as u32;
            checksum = checksum.wrapping_add(word);
        }
    }

    (checksum & 0xFFFF) as u16
}

/// Detect Genesis mapper from ROM size and header hints
pub fn detect_genesis_mapper(
    rom_size: usize,
    header: &retro_core::cartridge::detection::HeaderInfo,
) -> MapperId {
    // Check header hint first
    if let Some(ref mapper) = header.mapper_hint {
        return mapper.clone();
    }

    // Heuristics based on size
    if rom_size > 0x400000 {
        // > 4MB needs mapper
        MapperId::GenesisSEGA
    } else {
        // Standard ROM
        MapperId::GenesisNone
    }
}

/// Parse Genesis region string (e.g., "JUE" = Japan, USA, Europe)
pub fn parse_genesis_regions(region_str: &str) -> Vec<Region> {
    let mut regions = Vec::new();

    for c in region_str.chars() {
        match c {
            'J' => regions.push(Region::Japan),
            'U' => regions.push(Region::USA),
            'E' => regions.push(Region::Europe),
            'A' => regions.push(Region::Asia),
            'B' => regions.push(Region::Brazil),
            'K' => regions.push(Region::Korea),
            'C' => regions.push(Region::China),
            // New-style codes: one hex digit whose bits are markets
            // (1 = Japan, 4 = Americas, 8 = Europe). The single-bit
            // digits are unambiguous; a combined digit (`A`, `C`, `F`)
            // collides with an old-style letter and is left to the
            // old-style arm above.
            '1' => regions.push(Region::Japan),
            '4' => regions.push(Region::USA),
            '8' => regions.push(Region::Europe),
            _ => {}
        }
    }

    if regions.is_empty() {
        regions.push(Region::World);
    }

    regions
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_genesis_rom(size: usize) -> Vec<u8> {
        let mut rom = vec![0; size.max(0x200)];
        rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");
        rom
    }

    #[test]
    fn test_parse_genesis_regions() {
        let regions = parse_genesis_regions("JUE");
        assert_eq!(regions.len(), 3);
        assert!(regions.contains(&Region::Japan));
        assert!(regions.contains(&Region::USA));
        assert!(regions.contains(&Region::Europe));
    }

    #[test]
    fn test_parse_genesis_regions_empty() {
        let regions = parse_genesis_regions("");
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0], Region::World);
    }

    #[test]
    fn test_parse_genesis_regions_single() {
        let regions = parse_genesis_regions("U");
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0], Region::USA);
    }

    #[test]
    fn test_parse_genesis_regions_new_style_single_codes() {
        assert_eq!(parse_genesis_regions("1"), vec![Region::Japan]);
        assert_eq!(parse_genesis_regions("4"), vec![Region::USA]);
        assert_eq!(parse_genesis_regions("8"), vec![Region::Europe]);
        // Unknown characters still fall through to World.
        assert_eq!(parse_genesis_regions("F"), vec![Region::World]);
    }

    #[test]
    fn test_parse_genesis_regions_with_extras() {
        let regions = parse_genesis_regions("JUAB");
        assert!(regions.contains(&Region::Japan));
        assert!(regions.contains(&Region::USA));
        assert!(regions.contains(&Region::Asia));
        assert!(regions.contains(&Region::Brazil));
    }

    #[test]
    fn test_calculate_genesis_checksum() {
        let rom = create_test_genesis_rom(0x1000);
        let checksum = calculate_genesis_checksum(&rom);

        // Checksum should be deterministic
        let checksum2 = calculate_genesis_checksum(&rom);
        assert_eq!(checksum, checksum2);
    }

    #[test]
    fn test_detect_no_mapper_small_rom() {
        use retro_core::cartridge::detection::HeaderInfo;

        let header = HeaderInfo {
            title: "Small ROM".to_string(),
            serial: None,
            regions: vec![Region::USA],
            rom_size: 0x80000,
            sram_size: 0,
            sram_type: SRAMType::None,
            checksum: None,
            mapper_hint: None,
            expansion_chip_hint: None,
            io_support: vec![],
        };

        let mapper = detect_genesis_mapper(0x80000, &header);
        assert_eq!(mapper, MapperId::GenesisNone);
    }

    #[test]
    fn test_detect_sega_mapper_large_rom() {
        use retro_core::cartridge::detection::HeaderInfo;

        let header = HeaderInfo {
            title: "Large ROM".to_string(),
            serial: None,
            regions: vec![Region::USA],
            rom_size: 0x500000,
            sram_size: 0,
            sram_type: SRAMType::None,
            checksum: None,
            mapper_hint: None,
            expansion_chip_hint: None,
            io_support: vec![],
        };

        let mapper = detect_genesis_mapper(0x500000, &header);
        assert_eq!(mapper, MapperId::GenesisSEGA);
    }
}
