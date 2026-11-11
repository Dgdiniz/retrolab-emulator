//! ROM preset detection (LoROM vs HiROM vs ExHiROM).
//!
//! [`detect_preset`] walks the five [`HEADER_CANDIDATES`] offsets,
//! parses whatever it finds, and scores each candidate (the idea of
//! scoring header candidates is common to SNES emulators; the terms and
//! weights here are our own). The highest scorer wins, but only
//! if it crosses a confidence floor (default 70 points), below
//! that the caller should ask the user (or the manifest) what
//! mapper to use instead of guessing.
//!
//! The scoring vocabulary is deliberately small. A *correct* header
//! at the *right* offset earns ~140 points (`+100` reset vector,
//! `+20` map-mode coherence, `+10..21` title, `+6` checksum,
//! `+1` cart type, `+5` rom-size match). A garbage offset earns
//! at most a handful of points by luck, well below the floor. The
//! threshold of 70 leaves ~50 points of slack so games with weird
//! reset vectors (homebrew, hacked translations) still detect
//! reliably, while zero-filled buffers consistently miss.
//!
//! The function never returns [`PresetKind::Custom`]; that variant
//! is reserved for manifest-driven plans. SMC-padded dumps map to
//! the same kind as their non-padded twin (the SMC offset is
//! preserved in `header_offset`). [`PresetKind::Sdd1`] is returned
//! for a LoROM-shaped header whose `(cartridge_type, map_mode)`
//! identifier matches an S-DD1 cartridge (see [`SnesHeader::is_sdd1`]);
//! scoring stays LoROM-based, only the reported kind changes.

use crate::cartridge::header::{try_parse_header, SnesHeader, HEADER_CANDIDATES};
use crate::cartridge::presets::PresetKind;

/// Confidence floor for [`detect_preset`]. Below this, the function
/// returns `None` instead of a guess. Selected so that a header
/// with a logical reset vector AND map-mode coherence already
/// passes (the two single biggest signals) but raw garbage doesn't.
pub const DEFAULT_DETECT_THRESHOLD: i32 = 70;

/// Result of a successful detection.
#[derive(Debug, Clone)]
pub struct DetectedPreset {
    /// Stock mapper kind for the detected header.
    pub kind: PresetKind,
    /// Absolute byte offset in the ROM image where the matched
    /// header starts. Preserves the SMC-padded vs raw distinction
    /// (`$81B0` for SMC LoROM, `$101B0` for SMC HiROM).
    pub header_offset: usize,
    /// Heuristic score earned by this candidate.
    pub score: i32,
    /// Parsed header bytes, exposed so the caller can drive
    /// title / region / SRAM-size decisions without re-reading the
    /// ROM.
    pub header: SnesHeader,
    /// FastROM flag (`map_mode` bit 4) lifted out for convenience.
    pub fastrom: bool,
}

/// Score a candidate header. Returns `i32::MIN` when the offset
/// doesn't parse (no header at all) so it never wins a tie.
///
/// `expected_kind` is what the candidate offset *should* be:
/// `LoRom` for `$7FB0`/`$81B0`, `HiRom` for `$FFB0`/`$101B0`,
/// `ExHiRom` for `$40FFB0`. The map-mode coherence bonus checks
/// the parsed header against this expectation.
pub fn score_header(rom: &[u8], offset: usize, expected_kind: PresetKind) -> i32 {
    let Some(header) = try_parse_header(rom, offset) else {
        return i32::MIN;
    };
    score_parsed_header(&header, rom.len(), expected_kind)
}

fn score_parsed_header(header: &SnesHeader, rom_len: usize, expected_kind: PresetKind) -> i32 {
    let mut score: i32 = 0;

    let reset = header.vectors.emu_reset;
    if (0x8000..=0xFFFF).contains(&reset) {
        score += 100;
    } else if reset < 0x0100 {
        score -= 64;
    }

    if expected_map_mode_low(expected_kind) == Some(header.map_mode_low()) {
        score += 20;
    }

    score += header
        .title_bytes()
        .iter()
        .filter(|&&b| (0x20..=0x7E).contains(&b))
        .count() as i32;

    if header.checksum_is_consistent() {
        score += 6;
    }

    if (header.cartridge_type & 0x0F) <= 5 {
        score += 1;
    }

    if rom_size_within_15_percent(header.rom_size_bytes(), rom_len) {
        score += 5;
    }

    score
}

/// Iterate every candidate offset, score it, and return the
/// highest-scoring one, provided it crosses
/// [`DEFAULT_DETECT_THRESHOLD`].
pub fn detect_preset(rom: &[u8]) -> Option<DetectedPreset> {
    detect_preset_with_threshold(rom, DEFAULT_DETECT_THRESHOLD)
}

/// Like [`detect_preset`] but with a caller-chosen confidence
/// floor. Useful for fuzzing (set to `i32::MIN` to always accept
/// the best candidate, even garbage) or for stricter callers that
/// want headers to be obviously valid.
pub fn detect_preset_with_threshold(rom: &[u8], threshold: i32) -> Option<DetectedPreset> {
    let mut best: Option<DetectedPreset> = None;

    for &offset in &HEADER_CANDIDATES {
        let expected = expected_kind_for_offset(offset);
        let Some(header) = try_parse_header(rom, offset) else {
            continue;
        };
        let score = score_parsed_header(&header, rom.len(), expected);
        let fastrom = header.is_fastrom();
        // S-DD1 games (Star Ocean, SFA2) carry a LoROM-shaped header at
        // $7FB0 but a custom `cartridge_type`/`map_mode` identifier. The
        // LoROM-based score still clears the floor (the reset vector is the
        // dominant signal); we only promote the *preset kind* so the
        // builder generates the S-DD1 map instead of stock LoROM.
        let kind = if header.is_sdd1() {
            PresetKind::Sdd1
        } else if header.is_spc7110() {
            // SPC7110 games (Tengai Makyou Zero, Momotarou Dentetsu Happy,
            // Super Power League 4) carry a HiROM-shaped header at $FFB0 with
            // the custom map_mode $3A / cartridge_type $F5|$F9 identifier. The
            // HiROM-based score still clears the floor (the reset vector is the
            // dominant signal); we only promote the *preset kind* so the
            // builder generates the SPC7110 map instead of stock HiROM.
            PresetKind::Spc7110
        } else if header.is_sa1() {
            // SA-1 games (Super Mario RPG, Kirby Super Star, ...) carry a
            // LoROM-shaped header at $7FB0 with map_mode $23 and a
            // cartridge_type in the SA-1 sub-family. The LoROM-based score still
            // clears the floor; we only promote the *preset kind* so the builder
            // generates the SA-1 map instead of stock LoROM.
            PresetKind::Sa1
        } else if header.is_superfx() {
            // Super FX games (Star Fox, Doom, Yoshi's Island, ...) carry a
            // LoROM-shaped header at $7FB0 with map_mode $20, *identical* to
            // stock LoROM, and only the cartridge_type ({0x13,0x14,0x15,0x1A})
            // discriminates. The LoROM-based score is therefore a perfect match
            // and clears the floor; we only promote the *preset kind* so the
            // builder generates the Super FX map instead of stock LoROM.
            PresetKind::SuperFx
        } else if header.is_cx4() {
            // Cx4 games (Mega Man X2, Mega Man X3) also carry a LoROM-shaped
            // header at $7FB0 with map_mode $20, identical to stock LoROM, and
            // only the cartridge_type ($F3) discriminates. Same as the Super FX:
            // the LoROM-based score clears the floor and we only promote the
            // *preset kind* so the builder generates the Cx4 map.
            PresetKind::Cx4
        } else {
            expected
        };
        let candidate = DetectedPreset {
            kind,
            header_offset: offset,
            score,
            header,
            fastrom,
        };
        match best.as_ref() {
            Some(b) if b.score >= candidate.score => {}
            _ => best = Some(candidate),
        }
    }

    best.filter(|b| b.score >= threshold)
}

/// What stock mapper a candidate offset corresponds to. SMC
/// offsets (`$81B0`, `$101B0`) map to the same kind as their
/// non-padded twin, the `header_offset` retains the SMC distinction
/// for the cartridge builder to strip the 512-byte prelude if it
/// chooses.
fn expected_kind_for_offset(offset: usize) -> PresetKind {
    match offset {
        0x7FB0 | 0x81B0 => PresetKind::LoRom,
        0xFFB0 | 0x101B0 => PresetKind::HiRom,
        0x40_FFB0 => PresetKind::ExHiRom,
        // Defensive default; HEADER_CANDIDATES only contains the
        // five offsets above today.
        _ => PresetKind::LoRom,
    }
}

fn expected_map_mode_low(kind: PresetKind) -> Option<u8> {
    match kind {
        PresetKind::LoRom => Some(0x0),
        PresetKind::HiRom => Some(0x1),
        PresetKind::ExHiRom => Some(0x5),
        // S-DD1 carts declare map mode $32 (low nibble $2). Scoring uses
        // the LoROM offset expectation, so this arm is only consulted if a
        // caller scores explicitly against `Sdd1`.
        PresetKind::Sdd1 => Some(0x2),
        // SPC7110 carts declare map mode $3A (low nibble $A). Scoring uses
        // the HiROM offset expectation, so this arm is only consulted if a
        // caller scores explicitly against `Spc7110`.
        PresetKind::Spc7110 => Some(0xA),
        // SA-1 carts declare map mode $23 (low nibble $3). Scoring uses the
        // LoROM offset expectation, so this arm is only consulted if a caller
        // scores explicitly against `Sa1`.
        PresetKind::Sa1 => Some(0x3),
        // Super FX carts declare map mode $20 (low nibble $0), identical to
        // LoROM; the cartridge_type is what discriminates. Scoring already uses
        // the LoROM offset expectation, so this arm agrees with it.
        PresetKind::SuperFx => Some(0x0),
        // Cx4 carts declare map mode $20 (low nibble $0), identical to LoROM;
        // the cartridge_type ($F3) is what discriminates. Scoring uses the
        // LoROM offset expectation, so this arm agrees with it.
        PresetKind::Cx4 => Some(0x0),
        PresetKind::Custom => None,
    }
}

/// `true` iff `declared` is within +/-15% of `actual`. Pirated and
/// hacked ROMs frequently round the size code up to the next
/// power of two; the slack keeps those passing the bonus.
fn rom_size_within_15_percent(declared: u32, actual: usize) -> bool {
    if actual == 0 {
        return declared == 0;
    }
    let declared = declared as u64;
    let actual = actual as u64;
    let lower = actual * 85 / 100;
    let upper = actual * 115 / 100;
    (lower..=upper).contains(&declared)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cartridge::header::MAP_MODE_EXHIROM;

    /// Synthesize a ROM with a valid SNES header at the given
    /// offset. The header sets a reasonable reset vector
    /// (`$8000`), the requested map mode, a printable title, and a
    /// consistent checksum pair so the candidate scores near the
    /// "well-formed" ceiling.
    fn synth_rom_with_header(
        size: usize,
        offset: usize,
        map_mode: u8,
        rom_size_code: u8,
        reset_vector: u16,
    ) -> Vec<u8> {
        let mut rom = vec![0u8; size];
        stamp_header(&mut rom, offset, map_mode, rom_size_code, reset_vector);
        rom
    }

    fn stamp_header(
        rom: &mut [u8],
        offset: usize,
        map_mode: u8,
        rom_size_code: u8,
        reset_vector: u16,
    ) {
        let title_offset = offset + 0x10;
        let title = b"SCORING TEST  ROM    ";
        rom[title_offset..title_offset + 21].copy_from_slice(title);

        rom[offset + 0x25] = map_mode;
        rom[offset + 0x26] = 0x00;
        rom[offset + 0x27] = rom_size_code;
        rom[offset + 0x28] = 0x00;
        rom[offset + 0x29] = 0x01;
        rom[offset + 0x2A] = 0x01;
        rom[offset + 0x2B] = 0x00;

        let checksum: u16 = 0x1234;
        let complement = !checksum;
        rom[offset + 0x2C..offset + 0x2E].copy_from_slice(&complement.to_le_bytes());
        rom[offset + 0x2E..offset + 0x30].copy_from_slice(&checksum.to_le_bytes());

        rom[offset + 0x4C..offset + 0x4E].copy_from_slice(&reset_vector.to_le_bytes());
    }

    #[test]
    fn detect_lorom_synthetic_rom() {
        let rom = synth_rom_with_header(0x10_0000, 0x7FB0, 0x20, 0x0A, 0x8000);
        let detected = detect_preset(&rom).expect("LoROM detected");
        assert_eq!(detected.kind, PresetKind::LoRom);
        assert_eq!(detected.header_offset, 0x7FB0);
        assert!(detected.score >= 100, "score was {}", detected.score);
        assert!(!detected.fastrom);
    }

    #[test]
    fn detect_hirom_wins_over_lorom_when_header_in_ffb0() {
        // A 4 MiB image with a good HiROM header at $FFB0 and zero
        // bytes everywhere else (so the $7FB0 candidate scores
        // very low). The detector should pick HiROM.
        let rom = synth_rom_with_header(0x40_0000, 0xFFB0, 0x21, 0x0C, 0x8000);
        let detected = detect_preset(&rom).expect("HiROM detected");
        assert_eq!(detected.kind, PresetKind::HiRom);
        assert_eq!(detected.header_offset, 0xFFB0);

        // Sanity: the $7FB0 candidate parses (a zero-filled title
        // is accepted) but its reset vector is $0000, so it scores far
        // below the HiROM candidate and below the confidence floor,
        // it never wins.
        let lo = score_header(&rom, 0x7FB0, PresetKind::LoRom);
        let hi = score_header(&rom, 0xFFB0, PresetKind::HiRom);
        assert!(lo < hi, "zeroed LoROM offset {lo} must lose to HiROM {hi}");
        assert!(
            lo < DEFAULT_DETECT_THRESHOLD,
            "zeroed LoROM offset {lo} stays below the floor"
        );
    }

    #[test]
    fn detect_returns_none_below_threshold() {
        // A fully-zero buffer parses at every candidate that fits
        // (zero title bytes pass validation), but the $0000 reset
        // vector costs each one 64 points, so none crosses the
        // threshold.
        let rom = vec![0u8; 0x10_0000];
        assert!(detect_preset(&rom).is_none());
    }

    #[test]
    fn detect_brk_reset_vector_penalty() {
        // Reset vector at $0000 looks like a BRK pointer, the
        // header parses but the score loses 64 instead of gaining 100.
        let rom = synth_rom_with_header(0x10_0000, 0x7FB0, 0x20, 0x0A, 0x0000);
        let with_brk = score_header(&rom, 0x7FB0, PresetKind::LoRom);

        let mut ok_rom = rom.clone();
        ok_rom[0x7FB0 + 0x4C..0x7FB0 + 0x4E].copy_from_slice(&0x8000u16.to_le_bytes());
        let with_logical = score_header(&ok_rom, 0x7FB0, PresetKind::LoRom);

        // Same header otherwise -> the difference is exactly
        // +100 (logical bonus) vs -64 (BRK penalty) = 164 points.
        assert_eq!(with_logical - with_brk, 100 - -64);
    }

    #[test]
    fn detect_threshold_configurable() {
        // A reasonable LoROM header scores well over 100; bump the
        // threshold past that and the function rejects it.
        let rom = synth_rom_with_header(0x10_0000, 0x7FB0, 0x20, 0x0A, 0x8000);
        assert!(detect_preset_with_threshold(&rom, 200).is_none());
        assert!(detect_preset_with_threshold(&rom, 100).is_some());
        assert!(detect_preset_with_threshold(&rom, i32::MIN).is_some());
    }

    #[test]
    fn detect_exhirom_at_40ffb0() {
        // 6 MiB ROM (Tales-like) with header at $40FFB0 and
        // map_mode $25.
        let rom = synth_rom_with_header(0x60_0000, 0x40_FFB0, MAP_MODE_EXHIROM, 0x0D, 0x8000);
        let detected = detect_preset(&rom).expect("ExHiROM detected");
        assert_eq!(detected.kind, PresetKind::ExHiRom);
        assert_eq!(detected.header_offset, 0x40_FFB0);
    }

    #[test]
    fn score_header_returns_minimum_when_offset_doesnt_parse() {
        // A control byte in the title field makes `try_parse_header`
        // reject the offset outright, so the scorer returns `i32::MIN`
        // and the candidate can never win a tie. (A zero-filled title
        // is not enough to reject; see
        // `detect_blank_title_resolves_via_reset_vector`.)
        let mut rom = synth_rom_with_header(0x10_0000, 0x7FB0, 0x20, 0x0A, 0x8000);
        rom[0x7FB0 + 0x10] = 0x05; // control byte in the title
        assert_eq!(score_header(&rom, 0x7FB0, PresetKind::LoRom), i32::MIN);
    }

    #[test]
    fn detect_promotes_sdd1_kind_from_lorom_header() {
        // Star Ocean shape: LoROM header at $7FB0, reset $8000, but the
        // cartridge_type/map_mode identifier is $4532. The detector must
        // report `Sdd1` (not `LoRom`) while still clearing the floor.
        let mut rom = synth_rom_with_header(0x60_0000, 0x7FB0, 0x32, 0x0D, 0x8000);
        rom[0x7FB0 + 0x26] = 0x45; // cartridge_type -> identifier $4532
        let d = detect_preset(&rom).expect("S-DD1 detected");
        assert_eq!(d.kind, PresetKind::Sdd1);
        assert_eq!(d.header_offset, 0x7FB0);
        assert!(d.fastrom, "map_mode $32 sets the FastROM bit");
        assert!(
            d.score >= DEFAULT_DETECT_THRESHOLD,
            "S-DD1 score {} clears the floor on the LoROM-based signal",
            d.score
        );

        // A matching header with a stock LoROM identifier stays LoRom.
        let plain = synth_rom_with_header(0x10_0000, 0x7FB0, 0x20, 0x0A, 0x8000);
        assert_eq!(detect_preset(&plain).unwrap().kind, PresetKind::LoRom);
    }

    #[test]
    fn detect_promotes_spc7110_kind_from_hirom_header() {
        // Tengai Makyou Zero shape: HiROM header at $FFB0, reset $8000, but
        // the map_mode $3A / cartridge_type $F9 identifier is $F93A. The
        // detector must report `Spc7110` (not `HiRom`) while clearing the floor.
        let mut rom = synth_rom_with_header(0x40_0000, 0xFFB0, 0x3A, 0x0C, 0x8000);
        rom[0xFFB0 + 0x26] = 0xF9; // cartridge_type -> identifier $F93A (RTC)
        let d = detect_preset(&rom).expect("SPC7110 detected");
        assert_eq!(d.kind, PresetKind::Spc7110);
        assert_eq!(d.header_offset, 0xFFB0);
        assert!(d.fastrom, "map_mode $3A sets the FastROM bit");
        assert!(
            d.score >= DEFAULT_DETECT_THRESHOLD,
            "SPC7110 score {} must clear the floor",
            d.score
        );

        // The no-RTC identifier ($F53A) promotes to Spc7110 too.
        rom[0xFFB0 + 0x26] = 0xF5;
        assert_eq!(detect_preset(&rom).unwrap().kind, PresetKind::Spc7110);

        // A matching header with a stock HiROM identifier stays HiRom.
        let plain = synth_rom_with_header(0x40_0000, 0xFFB0, 0x21, 0x0C, 0x8000);
        assert_eq!(detect_preset(&plain).unwrap().kind, PresetKind::HiRom);
    }

    #[test]
    fn detect_promotes_sa1_kind_from_lorom_header() {
        // Super Mario RPG shape: LoROM header at $7FB0, reset $8000, map_mode
        // $23, cartridge_type $35. The detector must report `Sa1` (not `LoRom`)
        // while still clearing the floor on the LoROM-based signal.
        let mut rom = synth_rom_with_header(0x40_0000, 0x7FB0, 0x23, 0x0C, 0x8000);
        rom[0x7FB0 + 0x26] = 0x35; // cartridge_type -> SA-1 sub-family (battery)
        let d = detect_preset(&rom).expect("SA-1 detected");
        assert_eq!(d.kind, PresetKind::Sa1);
        assert_eq!(d.header_offset, 0x7FB0);
        assert!(!d.fastrom, "map_mode $23 leaves the FastROM bit clear");
        assert!(
            d.score >= DEFAULT_DETECT_THRESHOLD,
            "SA-1 score {} must clear the floor",
            d.score
        );

        // Every cartridge_type in the sub-family promotes to Sa1.
        for ct in [0x32, 0x34, 0x35, 0x36] {
            rom[0x7FB0 + 0x26] = ct;
            assert_eq!(detect_preset(&rom).unwrap().kind, PresetKind::Sa1);
        }

        // A matching header with a stock LoROM identifier stays LoRom.
        let plain = synth_rom_with_header(0x10_0000, 0x7FB0, 0x20, 0x0A, 0x8000);
        assert_eq!(detect_preset(&plain).unwrap().kind, PresetKind::LoRom);
    }

    #[test]
    fn detect_promotes_cx4_kind_from_lorom_header() {
        // Mega Man X2/X3 shape: LoROM header at $7FB0, reset $8000, map_mode $20
        // (identical to stock LoROM), cartridge_type $F3. The detector must
        // report `Cx4` (not `LoRom`) while still clearing the floor.
        let mut rom = synth_rom_with_header(0x20_0000, 0x7FB0, 0x20, 0x0B, 0x8000);
        rom[0x7FB0 + 0x26] = 0xF3; // cartridge_type -> Cx4
        let d = detect_preset(&rom).expect("Cx4 detected");
        assert_eq!(d.kind, PresetKind::Cx4);
        assert_eq!(d.header_offset, 0x7FB0);
        assert!(!d.fastrom, "map_mode $20 leaves the FastROM bit clear");
        assert!(
            d.score >= DEFAULT_DETECT_THRESHOLD,
            "Cx4 score {} clears the floor on the LoROM-based signal",
            d.score
        );

        // A stock LoROM identifier stays LoRom; a Super FX type stays SuperFx.
        let plain = synth_rom_with_header(0x10_0000, 0x7FB0, 0x20, 0x0A, 0x8000);
        assert_eq!(detect_preset(&plain).unwrap().kind, PresetKind::LoRom);
        rom[0x7FB0 + 0x26] = 0x13; // Super FX type
        assert_eq!(detect_preset(&rom).unwrap().kind, PresetKind::SuperFx);
    }

    #[test]
    fn detect_returns_smc_offset_for_padded_dump() {
        // An SMC-padded LoROM has its header at $81B0; the detected
        // preset reflects that so the cartridge builder can choose
        // whether to strip the 512-byte prelude.
        let mut rom = vec![0u8; 512 + 0x10_0000];
        stamp_header(&mut rom, 0x81B0, 0x20, 0x0A, 0x8000);
        let detected = detect_preset(&rom).expect("SMC LoROM detected");
        assert_eq!(detected.kind, PresetKind::LoRom);
        assert_eq!(detected.header_offset, 0x81B0);
    }

    #[test]
    fn detect_blank_title_resolves_via_reset_vector() {
        // The real-world case behind "no mapping: detector score below
        // threshold 70": diagnostic ROMs (Nintendo's SNES Test
        // Program, the SNES timer/speed tests) ship a
        // zero-filled title. The reset vector + map mode still pin the
        // mapping, so detection must succeed with the correct preset.

        // LoROM (the SNES Test Program's shape): header at $7FB0, reset
        // $8000, map mode $20, blank title.
        let mut lo = synth_rom_with_header(0x10_0000, 0x7FB0, 0x20, 0x0A, 0x8000);
        lo[0x7FB0 + 0x10..0x7FB0 + 0x10 + 21].fill(0x00);
        let d = detect_preset(&lo).expect("blank-title LoROM detected");
        assert_eq!(d.kind, PresetKind::LoRom);
        assert!(
            d.score >= DEFAULT_DETECT_THRESHOLD,
            "LoROM score {} must clear the floor",
            d.score
        );

        // HiROM (the timer/speed tests' shape): header at
        // $FFB0, reset $8733, map mode $31, blank title. The $7FB0
        // candidate has a $0000 reset and loses; HiROM wins.
        let mut hi = synth_rom_with_header(0x40_0000, 0xFFB0, 0x31, 0x0C, 0x8733);
        hi[0xFFB0 + 0x10..0xFFB0 + 0x10 + 21].fill(0x00);
        let d = detect_preset(&hi).expect("blank-title HiROM detected");
        assert_eq!(d.kind, PresetKind::HiRom);
        assert!(
            d.score >= DEFAULT_DETECT_THRESHOLD,
            "HiROM score {} must clear the floor",
            d.score
        );
    }
}
