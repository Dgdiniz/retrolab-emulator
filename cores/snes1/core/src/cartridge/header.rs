//! SNES cartridge header (`$xxFFB0..$xxFFFF`).
//!
//! Every SNES cartridge embeds an 80-byte header at one of five
//! well-known offsets, depending on the mapper and whether the dump
//! has a 512-byte SMC copier prelude:
//!
//! | Offset       | Mapper             |
//! |--------------|--------------------|
//! | `$00:7FB0`   | LoROM              |
//! | `$00:FFB0`   | HiROM              |
//! | `$40:FFB0`   | ExHiROM            |
//! | `$00:81B0`   | LoROM + SMC prelude  |
//! | `$00:101B0`  | HiROM + SMC prelude  |
//!
//! Each of those offsets points to a region laid out as
//! 16 bytes of extended/maker info, 32 bytes of "main" header
//! (title + meta + checksum), and 32 bytes of CPU vectors,
//! 80 bytes total. The exact byte layout within the header (relative
//! to `header_base`) is:
//!
//! | Offset     | Field                          |
//! |------------|--------------------------------|
//! | `+0x10`    | 21-byte title (padded `$20`)   |
//! | `+0x25`    | map mode                       |
//! | `+0x26`    | cartridge type                 |
//! | `+0x27`    | rom size code                  |
//! | `+0x28`    | sram size code                 |
//! | `+0x29`    | destination                    |
//! | `+0x2A`    | developer id                   |
//! | `+0x2B`    | rom version                    |
//! | `+0x2C-2D` | checksum complement (LE)       |
//! | `+0x2E-2F` | checksum (LE)                  |
//! | `+0x34-4F` | native + emulation vectors     |
//!
//! [`try_parse_header`] reads from a candidate offset and returns
//! `Some(SnesHeader)` only if the offset window has 80 bytes
//! available AND the data passes structural validation (title bytes
//! reasonable, `rom_size_code` within range). It deliberately does
//! *not* reject on checksum mismatch: hacked ROMs and translations
//! routinely ship with wrong checksums; that signal is used by the
//! scoring heuristic in [`super::detect`] instead of as a hard
//! gate.

/// Five candidate offsets where a SNES header can live, in priority
/// order: LoROM, HiROM, ExHiROM, then the two SMC-prelude variants.
pub const HEADER_CANDIDATES: [usize; 5] = [0x7FB0, 0xFFB0, 0x40_FFB0, 0x81B0, 0x101B0];

/// Map-mode low-nibble code for LoROM (mode 20h).
pub const MAP_MODE_LOROM: u8 = 0x00;
/// Map-mode low-nibble code for HiROM (mode 21h).
pub const MAP_MODE_HIROM: u8 = 0x01;
/// Map-mode low-nibble code for ExHiROM (mode 25h).
pub const MAP_MODE_EXHIROM: u8 = 0x05;

/// 80-byte SNES header at one of the candidate offsets.
///
/// Field offsets in comments are *relative to the header base*
/// (e.g. `+$25` is `$FFD5` for HiROM, `$7FD5` for LoROM).
#[derive(Debug, Clone)]
pub struct SnesHeader {
    /// 21-byte title rendered as `String` via lossy UTF-8 decode.
    /// JIS Shift-JIS titles (common on Japanese cartridges) will
    /// show replacement characters here; callers that need the raw
    /// bytes can use [`SnesHeader::title_bytes`].
    pub title: String,
    title_raw: [u8; 21],
    /// `+$25`: bits 0-3 carry the mapper mode (see
    /// [`MAP_MODE_LOROM`] / [`MAP_MODE_HIROM`] / [`MAP_MODE_EXHIROM`]);
    /// bit 4 is the FastROM flag; bit 5 is a sanity bit that should
    /// always read 1 on a well-formed header.
    pub map_mode: u8,
    /// `+$26`: bits 0-3 encode the chipset (ROM only, ROM+RAM,
    /// ROM+RAM+battery, ROM+coprocessor, ...); upper bits identify
    /// the coprocessor family for ROM+coprocessor cartridges.
    pub cartridge_type: u8,
    /// `+$27`: `rom_size_bytes = 1024 << rom_size_code`. Codes
    /// above `0x14` (1 GiB) are rejected by [`try_parse_header`].
    pub rom_size_code: u8,
    /// `+$28`: `sram_size_bytes = 1024 << sram_size_code`; code 0
    /// means "no SRAM".
    pub sram_size_code: u8,
    /// `+$0D`: extended-header expansion-RAM size code (`$7FBD` on a raw
    /// LoROM). Meaningful only when [`SnesHeader::developer_id`] is `$33` (an
    /// extended header is present); it carries the Game Pak RAM size for a
    /// Super FX cartridge, which the standard `+$28` SRAM byte reports as 0.
    /// See [`SnesHeader::superfx_ram_size`].
    pub expansion_ram_size_code: u8,
    /// `+$29`: region code (`0` = Japan, `1` = US/Canada,
    /// `2` = Europe PAL, ...).
    pub destination: u8,
    /// `+$2A`: developer / publisher id. Value `$33` is the
    /// extended-header escape (real id lives in the extended block
    /// at `+$00..+$01`); not unpacked here.
    pub developer_id: u8,
    /// `+$2B`: ROM revision number (usually `0`).
    pub rom_version: u8,
    /// `+$2C-2D`: bitwise complement of [`SnesHeader::checksum`].
    /// `checksum + checksum_complement` should equal `0xFFFF`.
    pub checksum_complement: u16,
    /// `+$2E-2F`: 16-bit sum of every byte in the cartridge
    /// (with the checksum field counted as `0xFF` twice). Hacked
    /// ROMs frequently ship with stale values here.
    pub checksum: u16,
    /// `+$34-4F`: native (65816) and emulation (6502-compat)
    /// interrupt vectors.
    pub vectors: SnesVectors,
}

/// Native + emulation interrupt vectors carried in the header
/// vectors block (`+$34..+$4F`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnesVectors {
    /// `+$34` (`$FFE4`), native COP instruction handler.
    pub native_cop: u16,
    /// `+$36` (`$FFE6`), native BRK instruction handler.
    pub native_brk: u16,
    /// `+$38` (`$FFE8`), native ABORT (unused on production
    /// hardware but listed by the official docs).
    pub native_abort: u16,
    /// `+$3A` (`$FFEA`), native NMI (V-blank interrupt).
    pub native_nmi: u16,
    /// `+$3E` (`$FFEE`), native IRQ (H/V timer interrupt).
    pub native_irq: u16,
    /// `+$44` (`$FFF4`), emulation-mode COP handler.
    pub emu_cop: u16,
    /// `+$48` (`$FFF8`), emulation-mode ABORT.
    pub emu_abort: u16,
    /// `+$4A` (`$FFFA`), emulation-mode NMI.
    pub emu_nmi: u16,
    /// `+$4C` (`$FFFC`), emulation-mode RESET. The CPU jumps
    /// here on cold boot; a sensible value pointing into
    /// `$8000-$FFFF` is the strongest scoring signal.
    pub emu_reset: u16,
    /// `+$4E` (`$FFFE`), emulation-mode IRQ / BRK.
    pub emu_irq: u16,
}

impl SnesHeader {
    /// FastROM flag (`map_mode` bit 4). FastROM lets `$80-$BF`
    /// banks run at 6 master cycles per access when MEMSEL is
    /// enabled; otherwise everything stays at 8.
    pub fn is_fastrom(&self) -> bool {
        (self.map_mode & 0x10) != 0
    }

    /// Low nibble of `map_mode` (LoROM = 0, HiROM = 1,
    /// ExHiROM = 5, ...). See the `MAP_MODE_*` constants.
    pub fn map_mode_low(&self) -> u8 {
        self.map_mode & 0x0F
    }

    /// `true` iff this header identifies an S-DD1 cartridge.
    ///
    /// The two S-DD1 cartridges are told by their header bytes (checked
    /// against snes9x): the
    /// `(cartridge_type << 8) | map_mode` identifier (raw `$FFD6`/`$FFD5`
    /// bytes) equals `0x4332` (Street Fighter Alpha 2 / Zero 2) or
    /// `0x4532` (Star Ocean). Both games map LoROM-style at the base, so
    /// the detector keeps scoring them as LoROM and only promotes the
    /// *preset kind* to S-DD1 on this signal.
    pub fn is_sdd1(&self) -> bool {
        let identifier = (u16::from(self.cartridge_type) << 8) | u16::from(self.map_mode);
        matches!(identifier, 0x4332 | 0x4532)
    }

    /// `true` iff this header identifies an SPC7110 cartridge.
    ///
    /// Told by its header bytes (checked against bsnes-plus): `map_mode`
    /// (`$FFD5`) `== 0x3A` and `cartridge_type` (`$FFD6`) `in {0xF5, 0xF9}`, i.e. the
    /// `(cartridge_type << 8) | map_mode` identifier is `0xF53A` (Momotarou
    /// Dentetsu Happy / Super Power League 4, no RTC) or `0xF93A` (Tengai
    /// Makyou Zero, with the RTC-4513). SPC7110 carts are HiROM-located (the
    /// header sits at `$FFB0`), so the detector keeps scoring them as HiROM
    /// and only promotes the *preset kind* to SPC7110 on this signal, the
    /// same approach [`SnesHeader::is_sdd1`] takes for LoROM-located S-DD1
    /// carts.
    pub fn is_spc7110(&self) -> bool {
        let identifier = (u16::from(self.cartridge_type) << 8) | u16::from(self.map_mode);
        matches!(identifier, 0xF53A | 0xF93A)
    }

    /// `true` iff this SPC7110 cartridge carries the RTC-4513 real-time clock
    /// (`cartridge_type == 0xF9`, i.e. only Tengai Makyou Zero among the three
    /// SPC7110 games). Meaningful only when [`SnesHeader::is_spc7110`] holds.
    pub fn has_spc7110_rtc(&self) -> bool {
        self.is_spc7110() && self.cartridge_type == 0xF9
    }

    /// `true` iff this header identifies an SA-1 cartridge.
    ///
    /// SA-1 carts declare `map_mode` (`$FFD5`) `== 0x23` (LoROM-located; the
    /// FastROM bit 4 is clear, so `$23 & 0x10 == 0` and [`SnesHeader::is_fastrom`]
    /// is `false`) and a `cartridge_type` (`$FFD6`) in `{0x32, 0x34, 0x35, 0x36}`
    /// (the SA-1 sub-family: `$32` plain, `$34`/`$35` with battery-backed
    /// BW-RAM, `$36` with un-batteried RAM). All six shipping SA-1 carts use
    /// `$23/$35`. Because the header sits at `$7FB0`, the detector keeps scoring
    /// these as LoROM and only promotes the *preset kind* to SA-1 on this
    /// signal, the same approach [`SnesHeader::is_sdd1`] takes for S-DD1 carts.
    pub fn is_sa1(&self) -> bool {
        self.map_mode == 0x23 && matches!(self.cartridge_type, 0x32 | 0x34 | 0x35 | 0x36)
    }

    /// `true` iff this SA-1 cartridge's BW-RAM is battery-backed
    /// (`cartridge_type in {0x34, 0x35}`). Meaningful only when
    /// [`SnesHeader::is_sa1`] holds; a `$36` cart has RAM that does not persist.
    pub fn sa1_has_battery(&self) -> bool {
        self.is_sa1() && matches!(self.cartridge_type, 0x34 | 0x35)
    }

    /// `true` iff this header identifies a Super FX / GSU cartridge.
    ///
    /// Unlike the SA-1 (`$23`), S-DD1 (`$32`), and SPC7110 (`$3A`), the Super
    /// FX declares the **same** `map_mode $20` a stock LoROM cart uses (Star
    /// Fox, Doom, and Yoshi's Island all sit at `$7FB0` with `map_mode $20`),
    /// so the only distinguishing signal is a `cartridge_type` (`$FFD6`) in the
    /// GSU sub-family `{0x13, 0x14, 0x15, 0x1A}`. The detector keeps scoring
    /// the cart as LoROM and only promotes the *preset kind* on this signal,
    /// the same approach [`SnesHeader::is_sdd1`] takes. Checked against
    /// bsnes-plus's header rules.
    pub fn is_superfx(&self) -> bool {
        self.map_mode == 0x20 && matches!(self.cartridge_type, 0x13 | 0x14 | 0x15 | 0x1A)
    }

    /// `true` iff this Super FX cartridge's Game Pak RAM is battery-backed
    /// (`cartridge_type == 0x15`, e.g. Yoshi's Island). Meaningful only when
    /// [`SnesHeader::is_superfx`] holds; Star Fox's (`$13`) 32 KiB is volatile
    /// scratch RAM that does not persist.
    pub fn superfx_has_battery(&self) -> bool {
        self.is_superfx() && self.cartridge_type == 0x15
    }

    /// Game Pak RAM size (bytes) for a Super FX cartridge. The standard
    /// `+$28` SRAM byte reads 0 on real GSU carts, so the size is taken
    /// from the extended header, the way snes9x does it (bsnes-plus looks
    /// the size up in its cartridge database instead). Meaningful only when
    /// [`SnesHeader::is_superfx`] holds:
    ///
    /// - **Extended header** (`developer_id == 0x33`, i.e. Doom / Yoshi's
    ///   Island), the size code is the expansion-RAM byte at `+$0D` (`$7FBD`),
    ///   *not* the standard SRAM byte. Doom declares `$06` (64 KiB), Yoshi's
    ///   Island `$05` (32 KiB). A code of 0 means "no RAM".
    /// - **No extended header** (`developer_id != 0x33`, i.e. Star Fox), the
    ///   byte is garbage, so the size is code 5 (32 KiB) of scratch RAM.
    ///
    /// The code is clamped to the 128 KiB GSU maximum (code 7) so a stray byte
    /// on a mislabelled cart can't request an absurd allocation.
    pub fn superfx_ram_size(&self) -> u32 {
        let code = if self.developer_id == 0x33 {
            self.expansion_ram_size_code
        } else {
            5
        };
        if code == 0 {
            0
        } else {
            1024u32 << code.min(7)
        }
    }

    /// `true` iff this header identifies a Cx4 (Hitachi HG51B169) cartridge.
    ///
    /// Like the Super FX, the Cx4 declares the **same** `map_mode $20` a stock
    /// LoROM cart uses, Mega Man X2 and X3 both sit at `$7FB0` with
    /// `map_mode $20`, so the only distinguishing signal is `cartridge_type`
    /// (`$FFD6`) `== 0xF3`. The detector keeps scoring the cart as LoROM and
    /// only promotes the *preset kind* on this signal, the same approach
    /// [`SnesHeader::is_sdd1`] takes. Checked against bsnes-plus's header
    /// rules.
    pub fn is_cx4(&self) -> bool {
        self.map_mode == 0x20 && self.cartridge_type == 0xF3
    }

    /// ROM size declared by the header (`1024 << rom_size_code`).
    pub fn rom_size_bytes(&self) -> u32 {
        1024u32 << self.rom_size_code
    }

    /// SRAM size declared by the header (`1024 << sram_size_code`),
    /// or 0 when the header says "no SRAM".
    pub fn sram_size_bytes(&self) -> u32 {
        if self.sram_size_code == 0 {
            0
        } else {
            1024u32 << self.sram_size_code
        }
    }

    /// `true` iff `checksum + checksum_complement == 0xFFFF`. A
    /// `false` here is informational, see the module docs.
    pub fn checksum_is_consistent(&self) -> bool {
        self.checksum.wrapping_add(self.checksum_complement) == 0xFFFF
    }

    /// Raw 21-byte title block, before lossy UTF-8 decoding. Useful
    /// for callers that want to apply a JIS Shift-JIS decoder.
    pub fn title_bytes(&self) -> &[u8; 21] {
        &self.title_raw
    }
}

/// Try to parse a header at `offset`. Returns `None` if the offset
/// window doesn't fit in `rom`, if the `rom_size_code` is out of
/// range, or if the title bytes look like garbage (any control byte in
/// `$01..$1F` or `$7F`). A zero-filled (all-NUL) title is accepted,
/// blank titles are common in homebrew / first-party test ROMs and a
/// header with a logical reset vector is still worth scoring.
/// Checksum mismatch is NOT a rejection reason, scoring uses it as a
/// soft signal.
pub fn try_parse_header(rom: &[u8], offset: usize) -> Option<SnesHeader> {
    if rom.len() < offset.checked_add(HEADER_TOTAL_LEN)? {
        return None;
    }
    let block = &rom[offset..offset + HEADER_TOTAL_LEN];

    let mut title_raw = [0u8; 21];
    title_raw.copy_from_slice(&block[TITLE_OFFSET..TITLE_OFFSET + 21]);
    if !title_bytes_reasonable(&title_raw) {
        return None;
    }
    // The display title strips the conventional trailing space / NUL
    // padding (the untouched 21 bytes stay available via
    // `title_bytes`). A fully blank, zero-filled title therefore
    // decodes to "", which the manifest exporter and ROM-info surface
    // report as "no title" rather than a row of NUL characters.
    let title = String::from_utf8_lossy(&title_raw)
        .trim_end_matches(|c: char| c == '\0' || c.is_whitespace())
        .to_string();

    let rom_size_code = block[ROM_SIZE_OFFSET];
    if rom_size_code > MAX_ROM_SIZE_CODE {
        return None;
    }

    let vectors = SnesVectors {
        native_cop: read_u16_le(block, NATIVE_COP_OFFSET),
        native_brk: read_u16_le(block, NATIVE_BRK_OFFSET),
        native_abort: read_u16_le(block, NATIVE_ABORT_OFFSET),
        native_nmi: read_u16_le(block, NATIVE_NMI_OFFSET),
        native_irq: read_u16_le(block, NATIVE_IRQ_OFFSET),
        emu_cop: read_u16_le(block, EMU_COP_OFFSET),
        emu_abort: read_u16_le(block, EMU_ABORT_OFFSET),
        emu_nmi: read_u16_le(block, EMU_NMI_OFFSET),
        emu_reset: read_u16_le(block, EMU_RESET_OFFSET),
        emu_irq: read_u16_le(block, EMU_IRQ_OFFSET),
    };

    Some(SnesHeader {
        title,
        title_raw,
        map_mode: block[MAP_MODE_OFFSET],
        cartridge_type: block[CART_TYPE_OFFSET],
        rom_size_code,
        sram_size_code: block[SRAM_SIZE_OFFSET],
        expansion_ram_size_code: block[EXPANSION_RAM_SIZE_OFFSET],
        destination: block[DESTINATION_OFFSET],
        developer_id: block[DEVELOPER_ID_OFFSET],
        rom_version: block[ROM_VERSION_OFFSET],
        checksum_complement: read_u16_le(block, CHECKSUM_COMPLEMENT_OFFSET),
        checksum: read_u16_le(block, CHECKSUM_OFFSET),
        vectors,
    })
}

const HEADER_TOTAL_LEN: usize = 0x50;
const EXPANSION_RAM_SIZE_OFFSET: usize = 0x0D;
const TITLE_OFFSET: usize = 0x10;
const MAP_MODE_OFFSET: usize = 0x25;
const CART_TYPE_OFFSET: usize = 0x26;
const ROM_SIZE_OFFSET: usize = 0x27;
const SRAM_SIZE_OFFSET: usize = 0x28;
const DESTINATION_OFFSET: usize = 0x29;
const DEVELOPER_ID_OFFSET: usize = 0x2A;
const ROM_VERSION_OFFSET: usize = 0x2B;
const CHECKSUM_COMPLEMENT_OFFSET: usize = 0x2C;
const CHECKSUM_OFFSET: usize = 0x2E;
const NATIVE_COP_OFFSET: usize = 0x34;
const NATIVE_BRK_OFFSET: usize = 0x36;
const NATIVE_ABORT_OFFSET: usize = 0x38;
const NATIVE_NMI_OFFSET: usize = 0x3A;
const NATIVE_IRQ_OFFSET: usize = 0x3E;
const EMU_COP_OFFSET: usize = 0x44;
const EMU_ABORT_OFFSET: usize = 0x48;
const EMU_NMI_OFFSET: usize = 0x4A;
const EMU_RESET_OFFSET: usize = 0x4C;
const EMU_IRQ_OFFSET: usize = 0x4E;

/// `1024 << 0x14` is 1 GiB, anything beyond that means the byte is
/// junk. Real cartridges top out at `0x0D` (~13 MiB / 100 Mbit).
const MAX_ROM_SIZE_CODE: u8 = 0x14;

fn read_u16_le(buf: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([buf[offset], buf[offset + 1]])
}

fn title_bytes_reasonable(title: &[u8; 21]) -> bool {
    title.iter().all(|&b| {
        // Printable ASCII, a high byte (probable JIS Shift-JIS), or
        // NUL. NUL is accepted because a great many homebrew and
        // first-party diagnostic ROMs (Nintendo's
        // SNES Test Program, the SNES timer/speed tests) ship a
        // zero-filled title field. Rejecting those discards an
        // otherwise-valid header, logical reset vector + coherent
        // map mode, and forces the user to hand-author a manifest.
        // Other control bytes ($01-$1F, $7F DEL) still reject, so a
        // garbage offset full of random low bytes is filtered cheaply
        // before it ever reaches the scorer.
        b == 0x00 || (0x20..=0x7E).contains(&b) || b >= 0x80
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal test ROM constructor: allocates `size` zero bytes
    /// then stamps a valid header at `offset`. The defaults produce
    /// a header that passes `try_parse_header` and scores well in
    /// `detect_preset` (checksum complement, title in ASCII,
    /// reset vector at `$8000`).
    fn build_rom_with_header(
        size: usize,
        offset: usize,
        map_mode: u8,
        rom_size_code: u8,
        title: &str,
    ) -> Vec<u8> {
        let mut rom = vec![0u8; size];
        write_header(
            &mut rom,
            offset,
            map_mode,
            0x00,
            rom_size_code,
            0x00,
            title,
            0x8000,
            0x8100,
        );
        rom
    }

    #[allow(clippy::too_many_arguments)]
    fn write_header(
        rom: &mut [u8],
        offset: usize,
        map_mode: u8,
        cartridge_type: u8,
        rom_size_code: u8,
        sram_size_code: u8,
        title: &str,
        emu_reset: u16,
        native_nmi: u16,
    ) {
        let block = &mut rom[offset..offset + HEADER_TOTAL_LEN];

        let mut title_buf = [0x20u8; 21]; // space padding
        for (i, b) in title.bytes().take(21).enumerate() {
            title_buf[i] = b;
        }
        block[TITLE_OFFSET..TITLE_OFFSET + 21].copy_from_slice(&title_buf);
        block[MAP_MODE_OFFSET] = map_mode;
        block[CART_TYPE_OFFSET] = cartridge_type;
        block[ROM_SIZE_OFFSET] = rom_size_code;
        block[SRAM_SIZE_OFFSET] = sram_size_code;
        block[DESTINATION_OFFSET] = 0x01; // USA
        block[DEVELOPER_ID_OFFSET] = 0x01;
        block[ROM_VERSION_OFFSET] = 0x00;
        let checksum: u16 = 0x1234;
        let complement = !checksum;
        block[CHECKSUM_COMPLEMENT_OFFSET..CHECKSUM_COMPLEMENT_OFFSET + 2]
            .copy_from_slice(&complement.to_le_bytes());
        block[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 2].copy_from_slice(&checksum.to_le_bytes());

        block[EMU_RESET_OFFSET..EMU_RESET_OFFSET + 2].copy_from_slice(&emu_reset.to_le_bytes());
        block[NATIVE_NMI_OFFSET..NATIVE_NMI_OFFSET + 2].copy_from_slice(&native_nmi.to_le_bytes());
    }

    #[test]
    fn parse_lorom_header_at_7fb0() {
        let rom = build_rom_with_header(0x10_0000, 0x7FB0, 0x20, 0x0A, "TEST LOROM");
        let header = try_parse_header(&rom, 0x7FB0).expect("LoROM header parses");
        assert_eq!(header.title.trim_end(), "TEST LOROM");
        assert_eq!(header.map_mode, 0x20);
        assert_eq!(header.map_mode_low(), MAP_MODE_LOROM);
        assert!(!header.is_fastrom());
        assert_eq!(header.rom_size_code, 0x0A);
        assert_eq!(header.rom_size_bytes(), 1024 << 0x0A);
        assert!(header.checksum_is_consistent());
        assert_eq!(header.vectors.emu_reset, 0x8000);
    }

    #[test]
    fn parse_hirom_header_at_ffb0() {
        let rom = build_rom_with_header(0x40_0000, 0xFFB0, 0x21, 0x0C, "TEST HIROM");
        let header = try_parse_header(&rom, 0xFFB0).expect("HiROM header parses");
        assert_eq!(header.map_mode_low(), MAP_MODE_HIROM);
        assert!(!header.is_fastrom());
        assert_eq!(header.rom_size_code, 0x0C);
    }

    #[test]
    fn parse_exhirom_header_at_40ffb0() {
        let rom = build_rom_with_header(0x60_0000, 0x40_FFB0, 0x25, 0x0D, "TEST EXHIROM");
        let header = try_parse_header(&rom, 0x40_FFB0).expect("ExHiROM header parses");
        assert_eq!(header.map_mode_low(), MAP_MODE_EXHIROM);
        assert_eq!(header.rom_size_code, 0x0D);
    }

    #[test]
    fn parse_lorom_smc_header_at_81b0() {
        // A 512-byte SMC copier header shifts everything up by 512
        // bytes, the SNES header that used to live at $7FB0 is
        // now at $81B0 from the raw file's perspective.
        let mut rom = vec![0u8; 512 + 0x10_0000];
        write_header(
            &mut rom,
            0x81B0,
            0x30,
            0x00,
            0x0A,
            0x00,
            "SMC LOROM FAST",
            0x8000,
            0x8100,
        );
        let header = try_parse_header(&rom, 0x81B0).expect("SMC LoROM parses");
        assert_eq!(header.map_mode, 0x30);
        assert!(header.is_fastrom());
    }

    #[test]
    fn parse_hirom_smc_header_at_101b0() {
        let mut rom = vec![0u8; 512 + 0x40_0000];
        write_header(
            &mut rom,
            0x101B0,
            0x31,
            0x00,
            0x0C,
            0x00,
            "SMC HIROM FAST",
            0x8000,
            0x8100,
        );
        let header = try_parse_header(&rom, 0x101B0).expect("SMC HiROM parses");
        assert_eq!(header.map_mode_low(), MAP_MODE_HIROM);
        assert!(header.is_fastrom());
    }

    #[test]
    fn parse_rejects_control_byte_title() {
        // A control byte ($01-$1F) or DEL ($7F) in the title still
        // looks like garbage (random code at the wrong offset), so the
        // parser keeps rejecting it, this is the cheap pre-filter
        // that stops a junk offset from reaching the scorer.
        for bad in [0x01u8, 0x1F, 0x7F] {
            let mut rom = build_rom_with_header(0x10_0000, 0x7FB0, 0x20, 0x0A, "TEST");
            rom[0x7FB0 + TITLE_OFFSET] = bad;
            assert!(
                try_parse_header(&rom, 0x7FB0).is_none(),
                "title byte {bad:#04x} must reject"
            );
        }
    }

    #[test]
    fn parse_accepts_blank_zero_filled_title() {
        // Many homebrew / first-party diagnostic ROMs leave the title
        // field zero-filled. As long as the rest of the header is sane
        // the parser must accept it (the detector then leans on the
        // reset vector + map mode). The display title decodes to "".
        let mut rom = build_rom_with_header(0x10_0000, 0x7FB0, 0x20, 0x0A, "IGNORED");
        for i in 0..21 {
            rom[0x7FB0 + TITLE_OFFSET + i] = 0x00;
        }
        let header = try_parse_header(&rom, 0x7FB0).expect("blank-title header parses");
        assert_eq!(header.title, "");
        assert!(header.title_bytes().iter().all(|&b| b == 0x00));
        assert_eq!(header.vectors.emu_reset, 0x8000);
    }

    #[test]
    fn parse_rejects_huge_rom_size_code() {
        let rom = build_rom_with_header(0x10_0000, 0x7FB0, 0x20, 0xFF, "TEST");
        assert!(try_parse_header(&rom, 0x7FB0).is_none());
    }

    #[test]
    fn vectors_parsed_correctly() {
        let mut rom = vec![0u8; 0x10_0000];
        write_header(
            &mut rom, 0x7FB0, 0x20, 0x00, 0x0A, 0x00, "VECTORS", 0xABCD, 0x1234,
        );
        let header = try_parse_header(&rom, 0x7FB0).unwrap();
        assert_eq!(header.vectors.emu_reset, 0xABCD);
        assert_eq!(header.vectors.native_nmi, 0x1234);
        // Untouched vectors default to 0.
        assert_eq!(header.vectors.native_cop, 0);
        assert_eq!(header.vectors.emu_irq, 0);
    }

    #[test]
    fn parse_returns_none_when_rom_too_short() {
        let rom = vec![0u8; 0x40]; // way smaller than offset + 0x50.
        assert!(try_parse_header(&rom, 0x7FB0).is_none());
    }

    #[test]
    fn sram_size_bytes_zero_for_code_zero() {
        let rom = build_rom_with_header(0x10_0000, 0x7FB0, 0x20, 0x0A, "TEST");
        let header = try_parse_header(&rom, 0x7FB0).unwrap();
        assert_eq!(header.sram_size_bytes(), 0);
    }

    #[test]
    fn detects_sdd1_by_identifier() {
        // Star Ocean: cartridge_type $45, map_mode $32 -> identifier $4532.
        let mut rom = build_rom_with_header(0x10_0000, 0x7FB0, 0x32, 0x0C, "STAR OCEAN");
        rom[0x7FB0 + CART_TYPE_OFFSET] = 0x45;
        let so = try_parse_header(&rom, 0x7FB0).unwrap();
        assert!(so.is_sdd1(), "Star Ocean identifier $4532 is S-DD1");

        // Street Fighter Alpha 2: type $43, mode $32 -> $4332.
        rom[0x7FB0 + CART_TYPE_OFFSET] = 0x43;
        let sfa2 = try_parse_header(&rom, 0x7FB0).unwrap();
        assert!(sfa2.is_sdd1(), "SFA2 identifier $4332 is S-DD1");

        // A plain LoROM (type $00, mode $20) is not S-DD1.
        let plain = try_parse_header(
            &build_rom_with_header(0x10_0000, 0x7FB0, 0x20, 0x0A, "PLAIN"),
            0x7FB0,
        )
        .unwrap();
        assert!(!plain.is_sdd1());
    }

    #[test]
    fn detects_spc7110_by_identifier() {
        // SPC7110 carts are HiROM-located ($FFB0), map_mode $3A.
        // Tengai Makyou Zero: cartridge_type $F9 -> identifier $F93A, has RTC.
        let mut rom = build_rom_with_header(0x40_0000, 0xFFB0, 0x3A, 0x0C, "TENGAI");
        rom[0xFFB0 + CART_TYPE_OFFSET] = 0xF9;
        let tmz = try_parse_header(&rom, 0xFFB0).unwrap();
        assert!(tmz.is_spc7110(), "TMZ identifier $F93A is SPC7110");
        assert!(tmz.has_spc7110_rtc(), "$F9 carts carry the RTC-4513");
        assert!(tmz.is_fastrom(), "map_mode $3A sets the FastROM bit");

        // Super Power League 4 / Momotarou: type $F5 -> $F53A, no RTC.
        rom[0xFFB0 + CART_TYPE_OFFSET] = 0xF5;
        let spl4 = try_parse_header(&rom, 0xFFB0).unwrap();
        assert!(spl4.is_spc7110(), "SPL4 identifier $F53A is SPC7110");
        assert!(!spl4.has_spc7110_rtc(), "$F5 carts have no RTC");

        // A plain HiROM (type $00, mode $21) is not SPC7110.
        let plain = try_parse_header(
            &build_rom_with_header(0x40_0000, 0xFFB0, 0x21, 0x0C, "PLAIN HI"),
            0xFFB0,
        )
        .unwrap();
        assert!(!plain.is_spc7110());
        assert!(!plain.has_spc7110_rtc());
    }

    #[test]
    fn detects_sa1_by_map_mode_and_type() {
        // SA-1 carts are LoROM-located ($7FB0), map_mode $23 (FastROM bit clear).
        // Super Mario RPG / Kirby Super Star: cartridge_type $35 (battery RAM).
        let mut rom = build_rom_with_header(0x40_0000, 0x7FB0, 0x23, 0x0C, "MARIO RPG");
        rom[0x7FB0 + CART_TYPE_OFFSET] = 0x35;
        let smrpg = try_parse_header(&rom, 0x7FB0).unwrap();
        assert!(smrpg.is_sa1(), "map_mode $23 + type $35 is SA-1");
        assert!(smrpg.sa1_has_battery(), "$35 carts have battery BW-RAM");
        assert!(
            !smrpg.is_fastrom(),
            "map_mode $23 leaves the FastROM bit clear"
        );

        // The whole SA-1 sub-family: $32 (plain, no battery), $34/$35 (battery),
        // $36 (RAM, no battery).
        for (ct, battery) in [(0x32, false), (0x34, true), (0x35, true), (0x36, false)] {
            rom[0x7FB0 + CART_TYPE_OFFSET] = ct;
            let h = try_parse_header(&rom, 0x7FB0).unwrap();
            assert!(h.is_sa1(), "type {ct:#04x} with map_mode $23 is SA-1");
            assert_eq!(h.sa1_has_battery(), battery, "battery flag for {ct:#04x}");
        }

        // A cartridge_type outside the sub-family is not SA-1 even at $23.
        rom[0x7FB0 + CART_TYPE_OFFSET] = 0x02;
        assert!(!try_parse_header(&rom, 0x7FB0).unwrap().is_sa1());

        // A plain LoROM (type $00, mode $20) is not SA-1.
        let plain = try_parse_header(
            &build_rom_with_header(0x40_0000, 0x7FB0, 0x20, 0x0A, "PLAIN LO"),
            0x7FB0,
        )
        .unwrap();
        assert!(!plain.is_sa1());
        assert!(!plain.sa1_has_battery());
    }

    #[test]
    fn detects_superfx_by_map_mode_and_type() {
        // Super FX carts are LoROM-located ($7FB0), map_mode $20, the SAME
        // mode a stock LoROM uses, so only the cartridge_type discriminates.
        // Star Fox: type $13, no extended header (developer_id != $33).
        let mut rom = build_rom_with_header(0x10_0000, 0x7FB0, 0x20, 0x0A, "STAR FOX");
        rom[0x7FB0 + CART_TYPE_OFFSET] = 0x13;
        let starfox = try_parse_header(&rom, 0x7FB0).unwrap();
        assert!(starfox.is_superfx(), "map_mode $20 + type $13 is Super FX");
        assert!(
            !starfox.superfx_has_battery(),
            "$13 is volatile scratch RAM"
        );
        // Star Fox lacks an extended header -> forced 32 KiB scratch RAM.
        assert_eq!(
            starfox.superfx_ram_size(),
            0x8000,
            "Star Fox quirk: no ext header forces 32 KiB RAM"
        );

        // The whole GSU sub-family: $13/$14/$1A (no battery), $15 (battery).
        for (ct, battery) in [(0x13, false), (0x14, false), (0x15, true), (0x1A, false)] {
            rom[0x7FB0 + CART_TYPE_OFFSET] = ct;
            let h = try_parse_header(&rom, 0x7FB0).unwrap();
            assert!(
                h.is_superfx(),
                "type {ct:#04x} with map_mode $20 is Super FX"
            );
            assert_eq!(
                h.superfx_has_battery(),
                battery,
                "battery flag for {ct:#04x}"
            );
        }

        // An extended-header cart (developer_id $33, e.g. Doom / Yoshi's
        // Island) takes its Game Pak RAM size from the expansion-RAM byte
        // ($7FBD), NOT the standard SRAM byte ($7FD8, which reads 0 on real
        // GSU carts). The standard byte stays 0 to prove it is ignored.
        rom[0x7FB0 + SRAM_SIZE_OFFSET] = 0x00;
        rom[0x7FB0 + CART_TYPE_OFFSET] = 0x15;
        rom[0x7FB0 + DEVELOPER_ID_OFFSET] = 0x33;
        rom[0x7FB0 + EXPANSION_RAM_SIZE_OFFSET] = 0x05; // Yoshi's Island: 32 KiB
        let yoshi = try_parse_header(&rom, 0x7FB0).unwrap();
        assert!(yoshi.is_superfx());
        assert!(yoshi.superfx_has_battery());
        assert_eq!(
            yoshi.superfx_ram_size(),
            1024 << 5,
            "expansion byte drives size"
        );

        // Doom (type $14, no battery) declares $06 -> 64 KiB of work RAM.
        rom[0x7FB0 + CART_TYPE_OFFSET] = 0x14;
        rom[0x7FB0 + EXPANSION_RAM_SIZE_OFFSET] = 0x06;
        let doom = try_parse_header(&rom, 0x7FB0).unwrap();
        assert!(
            !doom.superfx_has_battery(),
            "$14 Game Pak RAM is not battery"
        );
        assert_eq!(doom.superfx_ram_size(), 1024 << 6, "Doom: 64 KiB work RAM");

        // A code-0 expansion byte on an ext-header cart means "no RAM"; a
        // garbage code is clamped to the 128 KiB GSU maximum.
        rom[0x7FB0 + EXPANSION_RAM_SIZE_OFFSET] = 0x00;
        assert_eq!(
            try_parse_header(&rom, 0x7FB0).unwrap().superfx_ram_size(),
            0,
            "ext-header code-0 folds to no RAM"
        );
        rom[0x7FB0 + EXPANSION_RAM_SIZE_OFFSET] = 0xFF;
        assert_eq!(
            try_parse_header(&rom, 0x7FB0).unwrap().superfx_ram_size(),
            1024 << 7,
            "garbage expansion code clamps to 128 KiB"
        );

        // A plain LoROM (type $00, mode $20) is NOT Super FX despite the mode.
        let plain = try_parse_header(
            &build_rom_with_header(0x10_0000, 0x7FB0, 0x20, 0x0A, "PLAIN LO"),
            0x7FB0,
        )
        .unwrap();
        assert!(!plain.is_superfx());
        assert!(!plain.superfx_has_battery());
    }

    #[test]
    fn detects_cx4_by_map_mode_and_type() {
        // Cx4 carts are LoROM-located ($7FB0), map_mode $20, the SAME mode a
        // stock LoROM (and the Super FX) uses, so only the cartridge_type ($F3)
        // discriminates. Mega Man X2 shape.
        let mut rom = build_rom_with_header(0x18_0000, 0x7FB0, 0x20, 0x0B, "MEGAMAN X2");
        rom[0x7FB0 + CART_TYPE_OFFSET] = 0xF3;
        let mmx2 = try_parse_header(&rom, 0x7FB0).unwrap();
        assert!(mmx2.is_cx4(), "map_mode $20 + type $F3 is Cx4");

        // A plain LoROM (type $00, mode $20) is NOT Cx4 despite the mode.
        let plain = try_parse_header(
            &build_rom_with_header(0x10_0000, 0x7FB0, 0x20, 0x0A, "PLAIN LO"),
            0x7FB0,
        )
        .unwrap();
        assert!(!plain.is_cx4());

        // A Super FX cart (type $13, mode $20) is NOT Cx4, the $F3 type is what
        // separates the two same-map_mode chips.
        rom[0x7FB0 + CART_TYPE_OFFSET] = 0x13;
        let sfx = try_parse_header(&rom, 0x7FB0).unwrap();
        assert!(!sfx.is_cx4(), "Super FX ($13) is not Cx4");
        assert!(sfx.is_superfx());
    }

    #[test]
    fn title_bytes_round_trip() {
        let rom = build_rom_with_header(0x10_0000, 0x7FB0, 0x20, 0x0A, "RAW BYTES");
        let header = try_parse_header(&rom, 0x7FB0).unwrap();
        let bytes = header.title_bytes();
        // First chars match the title, rest is space-padded.
        assert_eq!(&bytes[..9], b"RAW BYTES");
        assert!(bytes[9..].iter().all(|&b| b == b' '));
    }
}
