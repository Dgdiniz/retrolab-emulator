//! What the Mega Drive gives the shared commands of `retro-cli`: its CPUs
//! and the parsers of their MAME-format lines, its start-up flag, the
//! 68000's optional timing columns, its `screenshot` flags (a pixel's
//! composition logged, VRAM patched) and its priority views, and the lines
//! of its profile.

use std::fmt::Write as _;
use std::io;
use std::path::Path;

use anyhow::Result;
use retro_cli::{CliCpu, Console, ProfileLine};
use retro_core::debug::{ByteOrder, TraceEntry, TraceText};
use retro_core::video::PixelImage;
use retro_core::{CpuArchitecture, EmulatorCore};
use sg1_core::debugger::{TraceEntryBinary, MAIN_CPU, SOUND_CPU, TRACE_ENTRY_SIZE};
use sg1_core::golden::SIX_BUTTON;
use sg1_core::vdp::RenderLayerMode;
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

/// The flags only the Mega Drive's `screenshot` takes.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct GenesisScreenshot {
    /// Log per-pixel composition data for a screen coordinate `X,Y`
    /// (e.g. `160,120`). Output goes to stderr.
    #[arg(long, value_parser = parse_debug_pixel)]
    pub debug_pixel: Option<(u16, u16)>,
    /// Patch VRAM word(s) before the last frame render (for debugging):
    /// after the frames run, the words are written and one more frame is
    /// drawn for the screenshot. Format: `ADDR=VALUE` (hex, e.g.
    /// `C6A6=825C`). Repeatable.
    #[arg(long, value_parser = parse_vram_patch)]
    pub patch_vram: Vec<(u16, u16)>,
}

/// A `--debug-pixel` coordinate, `X,Y`.
fn parse_debug_pixel(spec: &str) -> std::result::Result<(u16, u16), String> {
    let Some((x, y)) = spec.split_once(',') else {
        return Err("expected X,Y, e.g. 160,120".to_string());
    };
    let x = x.trim().parse().map_err(|_| format!("invalid X: {x}"))?;
    let y = y.trim().parse().map_err(|_| format!("invalid Y: {y}"))?;
    Ok((x, y))
}

/// A `--patch-vram` word, `ADDR=VALUE` in hex.
fn parse_vram_patch(spec: &str) -> std::result::Result<(u16, u16), String> {
    let Some((addr, value)) = spec.split_once('=') else {
        return Err("expected ADDR=VALUE".to_string());
    };
    let addr = u16::from_str_radix(addr.trim(), 16)
        .map_err(|_| format!("invalid hex address '{addr}'"))?;
    let value = u16::from_str_radix(value.trim(), 16)
        .map_err(|_| format!("invalid hex value '{value}'"))?;
    Ok((addr, value))
}

/// Write each patch's word into VRAM, big-endian at its byte address, as
/// the debugger writes it (a word in the sprite table reaches the VDP's
/// copy of it too), and say so on stderr; a patch past the end of VRAM is
/// left out.
fn patch_vram(emu: &mut GenesisEmulator, patches: &[(u16, u16)]) {
    for &(addr, value) in patches {
        if usize::from(addr) + 1 < emu.vdp().get_vram().len() {
            emu.vdp_mut()
                .write_vram_debug(u32::from(addr), &value.to_be_bytes());
            eprintln!("[patch-vram] addr=0x{addr:04X} = 0x{value:04X}");
        }
    }
}

impl Console for Genesis {
    type Emulator = GenesisEmulator;
    type BootArgs = GenesisBoot;
    type ScreenshotArgs = GenesisScreenshot;
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
            address_bits: 24,
            disassembly_bytes: 5,
            byte_order: ByteOrder::BigEndian,
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
            address_bits: 16,
            disassembly_bytes: 5,
            byte_order: ByteOrder::LittleEndian,
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
        "golden",
        "golden-set",
        "golden-compare",
        "manifest",
    ];
    const WORK_RAM: (u32, u32) = (0xFF_0000, 0x1_0000);
    const CHIP_ACTIVITY: &'static str = "the SVP's cycle count climbing on Virtua Racing";
    const VBLANK_INTERRUPT: &'static str = "VBlank interrupt";
    const PC_EXAMPLE: &'static str = "000400";
    const DECODED_WITH: &'static str = "";
    const WATCH_EXAMPLE: u32 = 0xFF_A190;
    const FRAME_INSTRUCTIONS: &'static str = "a Genesis frame is roughly 9k-14k 68000 instructions";
    const VIDEO_CHIP: &'static str = "VDP";
    const SOUND_CHIPS: &'static str = "YM2612 + PSG";
    const LAYER_NOTES: &'static str = "Besides the layers, the two priority views \
        `<stem>_priority` and `<stem>_sprite_priority`, which are opaque. `_plane_a` and \
        `_window` each show only their own part of the screen: the window replaces plane A where \
        it covers";
    const OVERSCAN_HELP: &'static str = "Changes nothing on the Mega Drive: its core shows no \
        overscan, the picture being the lines the VDP draws";
    const CHIP_STATE_EXAMPLE: &'static str = "the SVP's SSP1601 PC + cycle count on Virtua Racing";
    /// Work the core names otherwise (its cartridge's chips) counts in
    /// "Other/overhead", with the driver's own.
    const PROFILE: &'static [ProfileLine] = &[
        ProfileLine {
            label: "68K CPU",
            chip: "68000",
        },
        ProfileLine {
            label: "Z80 CPU",
            chip: "Z80",
        },
        ProfileLine {
            label: "VDP render",
            chip: "VDP",
        },
        ProfileLine {
            label: "YM2612",
            chip: "YM2612",
        },
        ProfileLine {
            label: "PSG",
            chip: "PSG",
        },
        ProfileLine {
            label: "Audio mix",
            chip: "audio mix",
        },
        ProfileLine {
            label: "Framebuffer",
            chip: "framebuffer",
        },
    ];
    const PROFILE_CLOCK: &'static str = "68K cycles";
    const BUTTONS_NOTE: &'static str = "`x`, `y`, `z` and `mode` reach the game only on 6-button \
        pads: `--six-button`, or a ROM whose header asks for them";
    const PRESS_EXAMPLE: &'static str = "";
    const RESET_EXAMPLE: &'static str = "";

    fn stage(_: &mut GenesisEmulator, _: &[u8], _: &GenesisBoot, _: bool) -> Result<()> {
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

    /// `--six-button` is the golden manifests' `six-button`.
    fn golden_flags(boot: &GenesisBoot) -> Vec<&'static str> {
        match boot.six_button {
            true => vec![SIX_BUTTON],
            false => Vec::new(),
        }
    }

    /// The file's path and size, then the header's title, region, checksum
    /// and mapper.
    fn write_rom_header(
        emu: &GenesisEmulator,
        rom: &[u8],
        path: &Path,
        out: &mut dyn io::Write,
    ) -> io::Result<()> {
        writeln!(out, "ROM: {}", path.display())?;
        writeln!(out, "Size: {} bytes ({} KB)", rom.len(), rom.len() / 1024)?;
        if let Some(info) = emu.get_rom_info() {
            writeln!(out)?;
            writeln!(out, "ROM Information:")?;
            writeln!(out, "  Title: {}", info.title.trim())?;
            writeln!(out, "  Region: {}", info.region)?;
            writeln!(out, "  Checksum: 0x{:04X}", info.checksum)?;
            if let Some(mapper) = &info.mapper {
                writeln!(out, "  Mapper: {mapper}")?;
            }
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

    /// The pixel `--debug-pixel` names logs its composition from the
    /// run's last frame on.
    fn before_last_frame(emu: &mut GenesisEmulator, args: &GenesisScreenshot) -> Result<()> {
        if let Some((x, y)) = args.debug_pixel {
            emu.vdp_mut().set_debug_pixel(x, y);
        }
        Ok(())
    }

    /// The VRAM patches, drawn by one more frame; then the pixel's log
    /// stops, before the layers and the priority views are drawn again.
    fn after_frames(emu: &mut GenesisEmulator, args: &GenesisScreenshot) -> Result<()> {
        if !args.patch_vram.is_empty() {
            retro_cli::cmd::screenshot::draw_again(emu, |emu| patch_vram(emu, &args.patch_vram));
        }
        emu.vdp_mut().clear_debug_pixel();
        Ok(())
    }

    /// The two priority views of the held frame: the layers by priority,
    /// and the sprites' priority.
    fn extra_layer_images(emu: &mut GenesisEmulator) -> Vec<(&'static str, PixelImage)> {
        [
            (RenderLayerMode::PriorityOverlay, "priority"),
            (RenderLayerMode::SpritePriority, "sprite_priority"),
        ]
        .into_iter()
        .map(|(mode, suffix)| (suffix, emu.vdp_mut().render_mode_image(mode)))
        .collect()
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
    fn a_manifest_the_core_refuses_names_its_file() {
        let path =
            std::env::temp_dir().join(format!("sg1-cli-manifest-{}.json", std::process::id()));
        std::fs::write(&path, r#"{ "core": "sg1", "version": "2.0" "#).unwrap();
        let file = retro_cli::rom::ManifestFile::read(&path).unwrap();
        let err = file.parse::<GenesisEmulator>().unwrap_err().to_string();
        std::fs::remove_file(&path).unwrap();
        assert!(
            err.starts_with(&format!("{}: invalid JSON", path.display())),
            "{err}"
        );
        assert_eq!(
            Genesis::golden_flags(&GenesisBoot { six_button: true }),
            vec![SIX_BUTTON]
        );
        assert!(Genesis::golden_flags(&GenesisBoot::default()).is_empty());
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

    #[test]
    fn the_rom_comes_first_where_a_command_takes_two_arguments() {
        let usage = |name: &str| {
            retro_cli::command::<Genesis>()
                .find_subcommand_mut(name)
                .expect(name)
                .render_usage()
                .to_string()
        };
        assert!(
            usage("memory").ends_with("<ROM> <ADDR>"),
            "{}",
            usage("memory")
        );
        assert!(usage("compare-trace").ends_with("<ROM> <REFERENCE>"));
        assert!(usage("compare").ends_with("<TRACE1> <TRACE2>"));
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
        assert!(help_of("info", "frames")
            .ends_with("e.g. the SVP's cycle count climbing on Virtua Racing"));
        assert!(help_of("run", "until_vblank").ends_with("the VBlank interrupt not yet raised"));
        assert_eq!(
            help_of("run", "until_pc"),
            "Step until the 68000 reaches this 24-bit PC (hex, e.g. `000400`)"
        );
        assert_eq!(
            help_of("disasm", "frames"),
            "Run this many frames before decoding (so PC settles)"
        );
        assert_eq!(
            help_of("memory", "addr"),
            "Start address (24-bit hex, e.g. `ff0000`, `0xFF0000`, `$FF0000`)"
        );
        assert_eq!(
            help_of("watch-ram", "addr"),
            "Work-RAM address to watch (24-bit hex, e.g. `FFA190`, `0xFFA190`)"
        );
        assert!(help_of("watch-ram", "press_at").ends_with(
            "`--warmup-frames` only: the \
            single-stepped window has no frame boundary to apply it at"
        ));
        assert!(help_of("run", "press").ends_with(
            "a run of `--instructions` or `--until-pc` \
            has no frame boundary to apply it at"
        ));
        assert_eq!(
            help_of("fingerprint", "len"),
            "Region length in bytes (default `10000` = the whole 64 KB Work RAM)"
        );
        assert!(help_of("screenshot", "press_at").contains(
            "from `up,down,left,right,a,b,c,start,x,y,z,mode`; `x`, `y`, `z` and `mode` reach the \
             game only on 6-button pads: `--six-button`, or a ROM whose header asks for them)"
        ));
        assert!(help_of("screenshot", "show_only")
            .contains("(`0`=Plane A, `1`=Plane B, `2`=Window and `3`=Sprites)"));
        assert!(help_of("screenshot", "render_layers").ends_with(
            "the two priority views `<stem>_priority` and `<stem>_sprite_priority`, which are \
             opaque. `_plane_a` and `_window` each show only their own part of the screen: the \
             window replaces plane A where it covers"
        ));
        assert!(help_of("screenshot", "overscan").starts_with("Changes nothing on the Mega Drive"));
        assert!(help_of("audio-dump", "press_at")
            .ends_with("so a capture without input records the boot jingle at best"));
    }

    #[test]
    fn the_cpu_table_names_each_cpu_once_in_the_slot_the_core_gives_it() {
        let emu = GenesisEmulator::new();
        let cpus = &emu.system_info().cpus;
        let mut names: Vec<&str> = Vec::new();
        for cpu in Genesis::CPUS {
            assert_eq!(cpus[cpu.id].architecture, cpu.architecture, "{}", cpu.label);
            assert_eq!(
                cpus[cpu.id].address_width, cpu.address_bits,
                "{}",
                cpu.label
            );
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

    /// A profile of `spans`, made the way the profiler makes one: through
    /// the hooks a core calls around its parts' work.
    fn profile_of(spans: &[(&'static str, u32)]) -> retro_core::frame::FrameProfile {
        use retro_core::frame::{Profiled, StepHooks};
        let mut profiler = Profiled::with_sample_period(1);
        for (chip, units) in spans {
            <Profiled as StepHooks<GenesisEmulator>>::chip_start(&mut profiler, chip);
            <Profiled as StepHooks<GenesisEmulator>>::chip_finish(&mut profiler, *units);
        }
        profiler.into_profile()
    }

    #[test]
    fn each_profile_line_is_found_by_the_name_the_core_gives_it() {
        use retro_cli::cmd::profile::ProfileSums;
        let a = profile_of(&[("68000", 3), ("Z80", 2), ("PSG", 0), ("68000", 1)]);
        let b = profile_of(&[("Z80", 4), ("68000", 2), ("cartridge", 0)]);
        let avg = ProfileSums::of(&[a, b], Genesis::PROFILE).average(2);
        assert_eq!(avg.units[0], 3, "68000: (3 + 1 + 2) / 2");
        assert_eq!(avg.units[1], 3, "Z80: (2 + 4) / 2");
        // Every line's part of the machine is one the core names.
        for line in Genesis::PROFILE {
            assert!(
                [
                    "68000",
                    "Z80",
                    "VDP",
                    "YM2612",
                    "PSG",
                    "audio mix",
                    "framebuffer"
                ]
                .contains(&line.chip),
                "{}",
                line.chip
            );
        }
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
