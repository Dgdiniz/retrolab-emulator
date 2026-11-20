//! What the Mega Drive gives the shared commands of `retro-cli`: its CPUs
//! and the parsers of their MAME-format lines, its start-up flag, and the
//! 68000's optional timing columns.

use std::fmt::Write as _;

use anyhow::Result;
use retro_cli::{CliCpu, Console};
use retro_core::debug::{TraceEntry, TraceText};
use retro_core::CpuArchitecture;
use sg1_core::debugger::{TraceEntryBinary, MAIN_CPU, SOUND_CPU, TRACE_ENTRY_SIZE};
use sg1_core::GenesisEmulator;

use crate::trace_ref;

/// The Mega Drive's command-line tool.
#[derive(Debug)]
pub struct Genesis;

/// The flag every command of the tool that loads a ROM takes.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct GenesisBoot {
    /// Connect 6-button pads so `x`/`y`/`z`/`mode` reach the game (the
    /// default is 3-button, matching MAME).
    #[arg(long)]
    pub six_button: bool,
}

impl Console for Genesis {
    type Emulator = GenesisEmulator;
    type BootArgs = GenesisBoot;
    type Command = crate::cmd::Command;

    const TOOL: &'static str = "sg1-cli";
    const ABOUT: &'static str = "SG1 Genesis/Mega Drive emulator command-line interface";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const ROM_FILES: &'static str = "`.bin` / `.md` / `.gen`";
    const IGNORE_EXAMPLE: &'static str = "PC,SR,A7";
    const TRACE_FORMAT: &'static str = "MAME";
    const TIMING_HELP: &'static str = "Prepend `TC:`(cumulative cycles)/`BY:`(scanline) \
        columns (68000 only) so cycle pacing is visible per instruction";
    const CPUS: &'static [CliCpu] = &[
        CliCpu {
            id: MAIN_CPU,
            names: &["m68k", "68k", "68000"],
            label: "68000",
            description: "68000",
            architecture: CpuArchitecture::M68000,
            timing_columns: "TC/FR/BX/BY",
            parse_line: trace_ref::parse_m68k,
            compared_with_own_trace: false,
            unavailable: None,
        },
        CliCpu {
            id: SOUND_CPU,
            names: &["z80"],
            label: "Z80",
            description: "",
            architecture: CpuArchitecture::Z80,
            timing_columns: "TC/FR/BX/BY",
            parse_line: trace_ref::parse_z80,
            compared_with_own_trace: false,
            unavailable: None,
        },
    ];
    const COMMAND_ORDER: &'static [&'static str] = &[
        "version",
        "info",
        "run",
        "trace",
        "compare-trace",
        "compare",
        "trace-to-text",
        "screenshot",
        "disasm",
        "memory",
        "dump-vdp",
        "watch-vram",
        "watch-ram",
        "watch-read",
        "fingerprint",
        "determinism-check",
        "benchmark",
        "profile",
        "audio-dump",
        "audio-info",
        "sound-trace",
        "savestate",
        "sram",
    ];

    fn stage(_: &mut GenesisEmulator, _: &[u8], _: &GenesisBoot) -> Result<()> {
        Ok(())
    }

    /// The load picks 6-button pads when the header asks for them;
    /// `--six-button` plugs them in for a ROM whose header does not.
    fn after_load(emu: &mut GenesisEmulator, boot: &GenesisBoot) -> Result<()> {
        if boot.six_button {
            emu.connect_controllers(true);
        }
        Ok(())
    }

    /// With `timing`, each 68000 line starts with its `TC:` (the cycles run
    /// before it) and `BY:` (the beam's line) columns, as MAME's do.
    fn trace_text(
        emu: &mut GenesisEmulator,
        cpu: &CliCpu,
        data: &[u8],
        timing: bool,
    ) -> Result<String> {
        if !(timing && cpu.id == MAIN_CPU) {
            return retro_cli::console::core_trace_text(emu, cpu, data);
        }
        let mut text = String::with_capacity(data.len() / TRACE_ENTRY_SIZE * 200);
        for chunk in data.as_chunks::<TRACE_ENTRY_SIZE>().0 {
            if let Some(entry) = TraceEntryBinary::from_bytes(chunk) {
                let _ = write!(
                    text,
                    "TC:{:016X} BY:{:04X} ",
                    entry.total_cycles, entry.scanline
                );
                entry.write_text(emu, &mut text);
                text.push('\n');
            }
        }
        Ok(text)
    }

    /// Without the ROM a 68000 line cannot be disassembled: its instruction
    /// column is the opcode word.
    fn trace_text_without_rom(cpu: &CliCpu, data: &[u8]) -> Option<String> {
        if cpu.id != MAIN_CPU {
            return None;
        }
        let mut text = String::with_capacity(data.len() / TRACE_ENTRY_SIZE * 200);
        for chunk in data.as_chunks::<TRACE_ENTRY_SIZE>().0 {
            if let Some(e) = TraceEntryBinary::from_bytes(chunk) {
                let _ = writeln!(
                    text,
                    "PC:{:06X} SR:{:04X} D0:{:08X} D1:{:08X} D2:{:08X} D3:{:08X} D4:{:08X} \
                     D5:{:08X} D6:{:08X} D7:{:08X} A0:{:08X} A1:{:08X} A2:{:08X} A3:{:08X} \
                     A4:{:08X} A5:{:08X} A6:{:08X} A7:{:08X} | DC.W ${:04X}",
                    e.pc,
                    e.sr,
                    e.d[0],
                    e.d[1],
                    e.d[2],
                    e.d[3],
                    e.d[4],
                    e.d[5],
                    e.d[6],
                    e.d[7],
                    e.a[0],
                    e.a[1],
                    e.a[2],
                    e.a[3],
                    e.a[4],
                    e.a[5],
                    e.a[6],
                    e.a[7],
                    e.opcode
                );
            }
        }
        Some(text)
    }

    fn run_command(command: crate::cmd::Command) -> Result<()> {
        crate::cmd::run(command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_cli::trace::line::{FieldValue, TraceLine};
    use retro_core::golden::GoldenConsole;
    use retro_core::EmulatorCore;
    use sg1_core::debugger::TraceEntryBinaryZ80;

    /// The number a parsed line holds in a field.
    fn number(line: &TraceLine, name: &str) -> Option<u64> {
        match line.field(name)?.value {
            FieldValue::Number(n) => Some(n),
            _ => None,
        }
    }

    #[test]
    fn the_command_line_is_well_formed_and_lists_every_command_in_order() {
        let cmd = retro_cli::command::<Genesis>();
        cmd.clone().debug_assert();
        let mut listed: Vec<&str> = cmd
            .get_subcommands()
            .map(|s| s.get_name())
            .filter(|name| *name != "help")
            .collect();
        listed.sort_unstable();
        let mut ordered = Genesis::COMMAND_ORDER.to_vec();
        ordered.sort_unstable();
        assert_eq!(listed, ordered);
    }

    /// The help text of `command`'s `--id`.
    fn help_of(command: &str, id: &str) -> String {
        let cmd = retro_cli::command::<Genesis>();
        let sub = cmd.find_subcommand(command).expect(command);
        let arg = sub.get_arguments().find(|a| a.get_id() == id).expect(id);
        arg.get_help().map(|h| h.to_string()).unwrap_or_default()
    }

    #[test]
    fn the_help_carries_the_notes_the_tool_gave() {
        assert_eq!(
            help_of("trace", "cpu"),
            "Which CPU to trace: `m68k` (68000) or `z80`"
        );
        for flag in ["press_at", "press_hold", "press", "reset_at"] {
            let help = help_of("trace", flag);
            assert!(help.ends_with("Frame-bounded runs only: a run of `--instructions` has no frame boundary to apply it at"), "{flag}: {help}");
        }
        assert!(help_of("compare-trace", "press_at").contains("reproduces presses timed by hand"));
        assert!(help_of("compare", "ignore").ends_with("(e.g. `PC,SR,A7`)"));
        assert!(help_of("compare-trace", "skip_timing")
            .contains("(the 68000's and the Z80's TC/FR/BX/BY)"));
        assert_eq!(
            help_of("trace-to-text", "rom"),
            "The ROM the trace was made from: the 68000's lines are disassembled from it \
             (without it, their instruction column is omitted); the Z80's need none"
        );
        assert!(!help_of("compare-trace", "cpu").contains("own trace"));
    }

    #[test]
    fn the_cpu_table_names_each_cpu_once_in_the_slot_the_core_gives_it() {
        let emu = GenesisEmulator::new();
        let cpus = &emu.system_info().cpus;
        let mut names: Vec<&str> = Vec::new();
        for cpu in Genesis::CPUS {
            assert_eq!(cpus[cpu.id].architecture, cpu.architecture, "{}", cpu.label);
            names.extend(cpu.names.iter().copied());
        }
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "a --cpu name is given twice");
    }

    #[test]
    fn the_press_flags_name_the_buttons_of_the_bits_they_always_had() {
        let bits: Vec<(&str, u32)> = vec![
            ("up", 0),
            ("down", 1),
            ("left", 2),
            ("right", 3),
            ("b", 4),
            ("c", 5),
            ("a", 6),
            ("start", 7),
            ("z", 8),
            ("y", 9),
            ("x", 10),
            ("mode", 11),
        ];
        for (name, bit) in bits {
            let found = <GenesisEmulator as GoldenConsole>::BUTTONS
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, b)| *b);
            assert_eq!(found, Some(bit), "{name}");
        }
    }

    #[test]
    fn the_68000_parser_reads_our_own_line_back() {
        let entry = TraceEntryBinary {
            pc: 0x000206,
            sr: 0x2708,
            d: [1, 2, 3, 4, 5, 6, 7, 0x8000_0000],
            a: [9, 10, 11, 12, 13, 14, 15, 0x00FF_FE00],
            total_cycles: 0x22,
            scanline: 0xE0,
            ..Default::default()
        };
        let mut emu = GenesisEmulator::new();
        let cpu = &Genesis::CPUS[0];
        for timing in [false, true] {
            let text =
                Genesis::trace_text(&mut emu, cpu, entry.to_bytes().as_ref(), timing).unwrap();
            let line = (cpu.parse_line)(text.trim_end()).expect("parses");
            assert_eq!(number(&line, "PC"), Some(0x206));
            assert_eq!(number(&line, "SR"), Some(0x2708));
            assert_eq!(number(&line, "D7"), Some(0x8000_0000));
            assert_eq!(number(&line, "A7"), Some(0x00FF_FE00));
            let by = line.field("BY").unwrap();
            if timing {
                assert_eq!(by.value, FieldValue::Number(0xE0));
                assert_eq!(number(&line, "TC"), Some(0x22));
            } else {
                assert_eq!(by.value, FieldValue::Missing);
            }
        }
    }

    #[test]
    fn the_z80_parser_reads_our_own_line_back() {
        let entry = TraceEntryBinaryZ80 {
            pc: 0x0008,
            af: 0x0040,
            sp: 0xFFFE,
            r_reg: 0x02,
            im: 1,
            iff1: 1,
            raw_bytes: [0xFB, 0, 0, 0],
            ..Default::default()
        };
        let mut text = String::new();
        entry.write_text(&mut (), &mut text);
        let line = (Genesis::CPUS[1].parse_line)(&text).expect("parses");
        assert_eq!(number(&line, "PC"), Some(0x0008));
        assert_eq!(number(&line, "AF"), Some(0x0040));
        assert_eq!(number(&line, "SP"), Some(0xFFFE));
        assert_eq!(number(&line, "R"), Some(0x02));
        assert_eq!(number(&line, "IM"), Some(1));
        assert_eq!(number(&line, "IFF1"), Some(1));
        assert_eq!(number(&line, "HALT"), Some(0));
    }

    #[test]
    fn a_68000_trace_without_its_rom_shows_the_opcode_word() {
        let entry = TraceEntryBinary {
            pc: 0x200,
            opcode: 0x4E71,
            ..Default::default()
        };
        let text =
            Genesis::trace_text_without_rom(&Genesis::CPUS[0], entry.to_bytes().as_ref()).unwrap();
        assert!(text.starts_with("PC:000200 SR:0000 D0:00000000"), "{text}");
        assert!(text.ends_with(" | DC.W $4E71\n"), "{text}");
        assert!(Genesis::trace_text_without_rom(&Genesis::CPUS[1], &[]).is_none());
    }
}
