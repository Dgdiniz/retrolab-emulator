//! The reports the inspection commands print: a hex dump, a disassembly
//! row, the cartridge's memory map and chips, a size in bytes and a CRC-32.

use std::io::{self, Write};

use retro_core::cartridge::{AccessType, ExpansionChipState, MemoryType};
use retro_core::{EmulatorCore, Instruction};

/// `data` as a hex dump, 16 bytes a row: `AAAAAA  hex bytes  |ascii|`, an
/// extra space between the row's two halves of 8, the address the row's
/// first (`base + row * 16`), a byte outside printable ASCII shown as `.`.
pub fn hex_dump(base: u32, data: &[u8]) -> String {
    // 16 bytes of "XX " and the space between the halves.
    const HEX_COLUMN: usize = 16 * 3 + 1;

    let mut out = String::new();
    for (row, chunk) in data.chunks(16).enumerate() {
        let addr = base.wrapping_add((row * 16) as u32);
        let mut hex = String::new();
        let mut ascii = String::new();
        for (i, &b) in chunk.iter().enumerate() {
            if i == 8 {
                hex.push(' ');
            }
            hex.push_str(&format!("{b:02x} "));
            ascii.push(if (0x20..=0x7e).contains(&b) {
                b as char
            } else {
                '.'
            });
        }
        while hex.len() < HEX_COLUMN {
            hex.push(' ');
        }
        out.push_str(&format!("{addr:06X}  {hex}|{ascii}|\n"));
    }
    out
}

/// One disassembled instruction: `addr  bytes  mnemonic operands`, then
/// `  ; comment` when it has one. The bytes column is as wide as
/// `column_bytes` bytes (`3 * column_bytes - 1` characters), so the
/// mnemonics of the instructions up to that length line up.
pub fn disassembly_row(insn: &Instruction, column_bytes: usize) -> String {
    let bytes: String = insn
        .bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ");
    let text = if insn.operands.is_empty() {
        insn.mnemonic.clone()
    } else {
        format!("{} {}", insn.mnemonic, insn.operands)
    };
    let width = (3 * column_bytes).saturating_sub(1);
    let mut row = format!("{:06x}  {bytes:<width$}  {text}", insn.address);
    if let Some(comment) = &insn.comment {
        row.push_str("  ; ");
        row.push_str(comment);
    }
    row
}

/// The memory map of the cartridge loaded: a blank line, `CPU memory map
/// (N regions):` and a line per region; nothing when it maps none.
pub fn write_memory_map<E: EmulatorCore>(emu: &mut E, out: &mut dyn Write) -> io::Result<()> {
    let map = emu.cartridge_debugger().get_memory_map();
    if map.regions.is_empty() {
        return Ok(());
    }
    writeln!(out)?;
    writeln!(out, "CPU memory map ({} regions):", map.regions.len())?;
    for r in &map.regions {
        writeln!(
            out,
            "  {:06x}-{:06x}  {:>10}  {:<12} {:<10} {}",
            r.cpu_address_start,
            r.cpu_address_end,
            human_bytes(r.size),
            memory_type_name(r.memory_type),
            access_name(r.access),
            r.description,
        )?;
    }
    Ok(())
}

/// The cartridge's chips: `Coprocessor: none` for a cartridge with none,
/// `Coprocessor: LABEL` for one, `Coprocessors: N` and a line per chip for
/// more; then each chip's state as the machine holds it now (at reset, or
/// after the frames a command ran).
pub fn write_chips<E: EmulatorCore>(emu: &mut E, out: &mut dyn Write) -> io::Result<()> {
    let debugger = emu.cartridge_debugger();
    match debugger.get_chip_labels().as_slice() {
        [] => return writeln!(out, "Coprocessor: none"),
        [(_, label)] => writeln!(out, "Coprocessor: {label}")?,
        many => {
            writeln!(out, "Coprocessors: {}", many.len())?;
            for (id, label) in many {
                writeln!(out, "  [{id}] {label}")?;
            }
        }
    }
    for (id, state) in debugger.get_chip_states() {
        write_chip_state(id, &state, out)?;
    }
    Ok(())
}

/// One chip's state: its type, program counter, cycle count, internal RAM
/// and registers.
fn write_chip_state(
    chip_id: u8,
    state: &ExpansionChipState,
    out: &mut dyn Write,
) -> io::Result<()> {
    writeln!(out)?;
    writeln!(out, "Coprocessor chip state [{chip_id}]:")?;
    writeln!(out, "  Type:           {:?}", state.chip_type)?;
    match state.program_counter {
        Some(pc) => writeln!(out, "  PC:             ${pc:04x}")?,
        None => writeln!(out, "  PC:             (idle - no program loaded)")?,
    }
    writeln!(out, "  Cycle count:    {}", state.cycle_count)?;
    match &state.internal_ram {
        Some(ram) => writeln!(
            out,
            "  Internal RAM:   {} bytes ({} x 16-bit words)",
            ram.len(),
            ram.len() / 2
        )?,
        None => writeln!(out, "  Internal RAM:   (none)")?,
    }
    if state.registers.is_empty() {
        writeln!(
            out,
            "  Registers:      (none - chip present but not driven)"
        )?;
    } else {
        writeln!(out, "  Registers:")?;
        for r in &state.registers {
            writeln!(out, "    {:<8} ${:04x}  {}", r.name, r.value, r.description)?;
        }
    }
    Ok(())
}

fn memory_type_name(t: MemoryType) -> &'static str {
    match t {
        MemoryType::ROM => "ROM",
        MemoryType::SRAM => "SRAM",
        MemoryType::CartridgeRAM => "CartRAM",
        MemoryType::ExpansionChipRAM => "ChipRAM",
        MemoryType::MappedIO => "MappedIO",
    }
}

fn access_name(a: AccessType) -> &'static str {
    match a {
        AccessType::ReadOnly => "ro",
        AccessType::WriteOnly => "wo",
        AccessType::ReadWrite => "rw",
        AccessType::MappedIO => "io",
    }
}

/// A size in bytes as `N MiB` or `N KiB` when it is a whole number of
/// them, else `N B`.
pub fn human_bytes(n: usize) -> String {
    const KIB: usize = 1024;
    const MIB: usize = 1024 * 1024;
    if n == 0 {
        "0 B".to_string()
    } else if n.is_multiple_of(MIB) {
        format!("{} MiB", n / MIB)
    } else if n.is_multiple_of(KIB) {
        format!("{} KiB", n / KIB)
    } else {
        format!("{n} B")
    }
}

/// The zlib CRC-32 of `data` (the reflected polynomial `0xEDB88320`, the
/// register started at `0xFFFFFFFF` and inverted at the end), so a value is
/// comparable with the same CRC another emulator takes of the same bytes.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = CRC32_TABLE[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

/// The CRC-32 of each byte value, for [`crc32`].
const CRC32_TABLE: [u32; 256] = crc32_table();

const fn crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut bit = 0;
        while bit < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            bit += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insn(bytes: &[u8], mnemonic: &str, operands: &str, comment: Option<&str>) -> Instruction {
        Instruction {
            address: 0x00_8002,
            bytes: bytes.to_vec(),
            mnemonic: mnemonic.to_string(),
            operands: operands.to_string(),
            comment: comment.map(str::to_string),
        }
    }

    #[test]
    fn a_dump_row_has_its_address_both_halves_and_its_ascii() {
        let data: Vec<u8> = (0..16).collect();
        let line = hex_dump(0x7E_0000, &data)
            .lines()
            .next()
            .unwrap()
            .to_string();
        assert!(line.starts_with("7E0000  00 01 02 03 04 05 06 07  08 09 0a 0b 0c 0d 0e 0f "));
        assert!(line.ends_with("|................|"));
        let text = hex_dump(0, b"Hello, SNES!");
        assert!(text.starts_with("000000  48 65 6c 6c 6f 2c 20 53"));
        assert!(text.trim_end().ends_with("|Hello, SNES!|"));
    }

    #[test]
    fn a_dump_short_row_keeps_the_ascii_column_and_later_rows_their_address() {
        let short = hex_dump(0, &[0x41, 0x42, 0x43]);
        let full = hex_dump(0, &[0u8; 16]);
        assert_eq!(short.find('|'), full.find('|'));
        assert!(short.trim_end().ends_with("|ABC|"));
        let rows = hex_dump(0x00_FFF8, &[0xAA; 20]);
        let mut lines = rows.lines();
        assert!(lines.next().unwrap().starts_with("00FFF8  aa"));
        assert!(lines.next().unwrap().starts_with("010008  aa aa aa aa "));
    }

    #[test]
    fn a_disassembly_row_pads_its_bytes_to_the_column_and_keeps_its_comment() {
        let sep = insn(&[0xe2, 0x30], "sep", "#$30", None);
        assert_eq!(disassembly_row(&sep, 4), "008002  e2 30        sep #$30");
        assert_eq!(disassembly_row(&sep, 5), "008002  e2 30           sep #$30");
        let bra = insn(&[0x80, 0x05], "bra", "$8009", Some("(+5)"));
        assert!(disassembly_row(&bra, 4).ends_with("bra $8009  ; (+5)"));
        let sei = insn(&[0x78], "sei", "", None);
        assert!(disassembly_row(&sei, 4).ends_with("  sei"));
    }

    #[test]
    fn sizes_read_in_the_largest_whole_unit() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(12), "12 B");
        assert_eq!(human_bytes(128 * 1024), "128 KiB");
        assert_eq!(human_bytes(4 * 1024 * 1024), "4 MiB");
        assert_eq!(human_bytes(4032 * 1024), "4032 KiB");
    }

    #[test]
    fn the_crc_is_zlib_s() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(
            crc32(b"The quick brown fox jumps over the lazy dog"),
            0x414F_A339
        );
    }
}
