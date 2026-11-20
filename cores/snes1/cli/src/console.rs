//! What the SNES gives the shared commands of `retro-cli`: its CPUs (a
//! cartridge's own among them) and the parsers of their bsnes-plus-format
//! lines, its start-up flags (the DSP-1's emulation and the forced mapping
//! of a ROM the detector doubts), the warnings about a blanked picture, and
//! the lines of its profile.

use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use retro_cli::report::human_bytes;
use retro_cli::{CliCpu, Console, ProfileLine};
use retro_core::debug::ByteOrder;
use retro_core::{CpuArchitecture, EmulatorCore};
use snes1_core::golden::FORCE_DETECT;
use snes1_core::{CoprocessorMode, PresetKind, SnesEmulator, CARTRIDGE_CPU, MAIN_CPU, SOUND_CPU};

use crate::trace_ref;

/// The SNES's command-line tool.
#[derive(Debug)]
pub struct Snes;

/// The flags every command of the tool that loads a ROM takes.
#[derive(Args, Clone, Debug, Default)]
pub struct SnesBoot {
    /// Load low-confidence ROMs (blank title / bad checksum) by forcing
    /// the vector-implied mapping instead of failing the detector gate.
    #[arg(long)]
    pub force_detect: bool,
    /// DSP-1 firmware + coprocessor-mode flags.
    #[command(flatten)]
    pub coprocessor: CoprocessorArgs,
}

/// The SNES's `screenshot` takes no flags of its own.
#[derive(Args, Clone, Debug, Default)]
pub struct SnesScreenshot {}

impl Console for Snes {
    type Emulator = SnesEmulator;
    type BootArgs = SnesBoot;
    type ScreenshotArgs = SnesScreenshot;
    type Command = crate::cmd::Command;

    const TOOL: &'static str = "snes1-cli";
    const ABOUT: &'static str = "SNES1 emulator command-line interface";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const ROM_FILES: &'static str = "`.sfc` / `.smc`";
    const IGNORE_EXAMPLE: &'static str = "A,X,DB,P,eff";
    const TRACE_FORMAT: &'static str = "bsnes-plus";
    const TIMING_HELP: &'static str = "Changes nothing on the SNES: the 65C816's lines carry \
        their V/H/F timing columns always, and the other CPUs' lines carry none";
    const CPUS: &'static [CliCpu] = &[
        CliCpu {
            id: MAIN_CPU,
            names: &["snes", "65816", "65c816", "5a22", "main", "cpu"],
            label: "65C816",
            description: "65C816",
            architecture: CpuArchitecture::Mos65816,
            address_bits: 24,
            disassembly_bytes: 4,
            byte_order: ByteOrder::LittleEndian,
            timing_columns: "V/H/F",
            parse_line: trace_ref::parse_65816,
            compared_with_own_trace: false,
            unavailable: None,
        },
        CliCpu {
            id: SOUND_CPU,
            names: &["spc700", "spc", "smp", "apu", "audio"],
            label: "SPC700",
            description: "",
            architecture: CpuArchitecture::Spc700,
            address_bits: 16,
            disassembly_bytes: 4,
            byte_order: ByteOrder::LittleEndian,
            timing_columns: "",
            parse_line: trace_ref::parse_spc700,
            compared_with_own_trace: false,
            unavailable: None,
        },
        CliCpu {
            id: CARTRIDGE_CPU,
            names: &["gsu", "superfx", "super-fx", "fx"],
            label: "GSU",
            description: "Super FX",
            architecture: CpuArchitecture::SuperFx,
            address_bits: 24,
            disassembly_bytes: 4,
            byte_order: ByteOrder::LittleEndian,
            timing_columns: "",
            parse_line: trace_ref::parse_gsu,
            compared_with_own_trace: true,
            unavailable: if cfg!(feature = "chip-superfx") {
                None
            } else {
                Some("this snes1-cli was built without the Super FX (feature `chip-superfx`)")
            },
        },
        CliCpu {
            id: CARTRIDGE_CPU,
            names: &["cx4", "hg51b", "hg51b169"],
            label: "Cx4",
            description: "Cx4 HG51B169",
            architecture: CpuArchitecture::Hg51b,
            address_bits: 24,
            disassembly_bytes: 4,
            byte_order: ByteOrder::LittleEndian,
            timing_columns: "",
            parse_line: trace_ref::parse_cx4,
            compared_with_own_trace: true,
            unavailable: if cfg!(feature = "chip-cx4") {
                None
            } else {
                Some("this snes1-cli was built without the Cx4 (feature `chip-cx4`)")
            },
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
        "determinism-check",
        "screenshot",
        "disasm",
        "ppu-dump",
        "apu-dump",
        "audio-dump",
        "singlestep-65816",
        "singlestep-spc700",
        "benchmark",
        "profile",
        "memory",
        "watch-ram",
        "fingerprint",
        "savestate",
        "sram",
        "golden",
        "golden-set",
        "golden-compare",
        "manifest",
    ];
    const WORK_RAM: (u32, u32) = (0x7E_0000, 0x2_0000);
    const CHIP_ACTIVITY: &'static str = "S-DD1 `DECOMPS > 0`";
    const VBLANK_INTERRUPT: &'static str = "NMI";
    const PC_EXAMPLE: &'static str = "00ff03";
    const DECODED_WITH: &'static str = "flags";
    const WATCH_EXAMPLE: u32 = 0x7E_0019;
    const FRAME_INSTRUCTIONS: &'static str = "a SNES frame is roughly 11k-14k 65C816 instructions";
    const VIDEO_CHIP: &'static str = "PPU";
    const SOUND_CHIPS: &'static str = "S-DSP";
    const LAYER_NOTES: &'static str = "";
    const OVERSCAN_HELP: &'static str = "Show the full overscan frame (240/480 tall) instead of \
        cropping to the 224-line NTSC TV window. Off by default, matching bsnes-plus / snes9x \
        output";
    const CHIP_STATE_EXAMPLE: &'static str = "S-DD1 `DECOMPS > 0` + DMA shadow";
    /// A cartridge's chips (the Super FX, the SA-1, the DSP-1) count in
    /// "Cartridge".
    const PROFILE: &'static [ProfileLine] = &[
        ProfileLine {
            label: "65C816 CPU",
            chip: "65C816",
        },
        ProfileLine {
            label: "DMA",
            chip: "DMA",
        },
        ProfileLine {
            label: "PPU render",
            chip: "PPU",
        },
        ProfileLine {
            label: "HDMA",
            chip: "HDMA",
        },
        ProfileLine {
            label: "APU",
            chip: "APU",
        },
        ProfileLine {
            label: "Audio mix",
            chip: "audio mix",
        },
        ProfileLine {
            label: "Framebuffer",
            chip: "framebuffer",
        },
        ProfileLine {
            label: "Cartridge",
            chip: "cartridge",
        },
    ];
    const PROFILE_CLOCK: &'static str = "master cycles";
    const BUTTONS_NOTE: &'static str = "";
    const PRESS_EXAMPLE: &'static str = "a Mode 5 menu";
    const RESET_EXAMPLE: &'static str = "the SPC7110 A/B self-test (press A -> reset -> press B \
        -> reset -> the game boots)";

    /// The DSP-1's emulation and firmware, then the forced mapping, which a
    /// manifest the command stages would replace.
    fn stage(emu: &mut SnesEmulator, rom: &[u8], boot: &SnesBoot, manifest: bool) -> Result<()> {
        if boot.force_detect && manifest {
            anyhow::bail!(
                "--force-detect and a cartridge manifest both choose the mapping; give one"
            );
        }
        boot.coprocessor.apply(emu)?;
        apply_force_detect(emu, rom, boot.force_detect);
        Ok(())
    }

    /// `--force-detect` is the golden manifests' `force-detect`.
    fn golden_flags(boot: &SnesBoot) -> Vec<&'static str> {
        match boot.force_detect {
            true => vec![FORCE_DETECT],
            false => Vec::new(),
        }
    }

    /// The file and its size, then the header: its title and how sure the
    /// detector is of it, the region, the mapping, the sizes, the vectors
    /// and the checksum.
    fn write_rom_header(
        emu: &SnesEmulator,
        rom: &[u8],
        path: &Path,
        out: &mut dyn io::Write,
    ) -> io::Result<()> {
        writeln!(out, "File:        {}", path.display())?;
        writeln!(
            out,
            "File size:   {} bytes ({})",
            rom.len(),
            human_bytes(rom.len())
        )?;
        if let Some(info) = emu.get_rom_info() {
            writeln!(
                out,
                "Title:       {:?} (confidence: {})",
                info.title.trim(),
                info.title_confidence
            )?;
            writeln!(out, "Region:      {}", info.region)?;
        }
        let mapper = emu.preset_kind().map_or("unknown", preset_name);
        let Some(h) = emu.header() else {
            return writeln!(out, "Mapper:      {mapper} (no parsed header)");
        };
        writeln!(
            out,
            "Mapper:      {mapper} (map_mode ${:02x}, low ${:x}){}",
            h.map_mode,
            h.map_mode_low(),
            if h.is_fastrom() {
                ", FastROM"
            } else {
                ", SlowROM"
            }
        )?;
        writeln!(
            out,
            "ROM size:    header {} ({}), file {}",
            h.rom_size_bytes(),
            human_bytes(h.rom_size_bytes() as usize),
            human_bytes(rom.len())
        )?;
        writeln!(
            out,
            "SRAM size:   {} ({})",
            h.sram_size_bytes(),
            human_bytes(h.sram_size_bytes() as usize)
        )?;
        writeln!(
            out,
            "Reset vec:   ${:04x}   NMI: native ${:04x} / emu ${:04x}",
            h.vectors.emu_reset, h.vectors.native_nmi, h.vectors.emu_nmi
        )?;
        writeln!(
            out,
            "IRQ vec:     native ${:04x} / emu ${:04x}",
            h.vectors.native_irq, h.vectors.emu_irq
        )?;
        writeln!(
            out,
            "Checksum:    ${:04x}  complement ${:04x}  [{}]",
            h.checksum,
            h.checksum_complement,
            if h.checksum_is_consistent() {
                "consistent"
            } else {
                "INCONSISTENT"
            }
        )
    }

    /// A frame in forced blank, or at zero brightness, is black: say so,
    /// so a black PNG is not taken for a capture that went wrong.
    fn picture_notes(emu: &SnesEmulator, frames: u64) -> Vec<String> {
        if emu.ppu().display_disable() {
            vec![format!(
                "PPU is in forced blank (display_disable set) after {frames} frames - the image \
                 will be black"
            )]
        } else if emu.ppu().display_brightness() == 0 {
            vec![format!(
                "PPU master brightness is 0 after {frames} frames - the image will be black"
            )]
        } else {
            Vec::new()
        }
    }

    fn run_command(command: crate::cmd::Command) -> Result<()> {
        crate::cmd::run(command)
    }
}

/// The name `info` gives a cartridge's mapping.
fn preset_name(kind: PresetKind) -> &'static str {
    match kind {
        PresetKind::LoRom => "LoROM",
        PresetKind::HiRom => "HiROM",
        PresetKind::ExHiRom => "ExHiROM",
        PresetKind::Sdd1 => "S-DD1",
        PresetKind::Spc7110 => "SPC7110",
        PresetKind::Sa1 => "SA-1",
        PresetKind::SuperFx => "Super FX",
        PresetKind::Cx4 => "Cx4",
        PresetKind::Custom => "Custom",
    }
}

/// When `force` is set, stage the threshold-free best-guess mapping as a
/// manifest override so a low-confidence ROM (blank title / bad checksum,
/// like Nintendo's SNES Test Program cartridge) loads with its vector-implied
/// preset instead of being rejected by the detector's confidence gate.
/// A no-op otherwise. Call before `load_rom`.
pub(crate) fn apply_force_detect(emu: &mut SnesEmulator, rom: &[u8], force: bool) {
    if force {
        let manifest = emu.detected_manifest(rom);
        emu.set_cartridge_manifest_override(Some(manifest));
    }
}

/// The coprocessor flags of the commands that boot a ROM, flattened into
/// each command's arguments.
#[derive(Args, Debug, Clone, Default)]
pub struct CoprocessorArgs {
    /// Path to a DSP-1 firmware dump (`dsp1b.rom` / `dsp1b.bin`, 8192
    /// bytes) enabling cycle-accurate low-level emulation of DSP-1 games
    /// such as Super Mario Kart (the `nec-lle` feature, on by default; a
    /// snes1-cli built without it ignores the dump, with a warning).
    /// Optional: without it those carts run the firmware-free
    /// high-level emulation (HLE), which renders the Mode-7 track
    /// correctly. Byte order (`.bin` BE / `.rom` LE) and revision are
    /// auto-detected by SHA-256.
    #[arg(long, value_name = "PATH")]
    pub dsp1_firmware: Option<PathBuf>,
    /// How a detected coprocessor is emulated: `auto` (LLE when firmware
    /// is present and built in, otherwise the firmware-free HLE), `lle`
    /// (force low-level, falls back to a non-hanging idle stub if
    /// firmware is missing/invalid; refused by a build without the
    /// `nec-lle` feature), or `hle` (force the firmware-free HLE).
    #[arg(
        long,
        value_name = "MODE",
        default_value = "auto",
        value_parser = parse_coprocessor_mode
    )]
    pub coprocessor_mode: CoprocessorMode,
}

fn parse_coprocessor_mode(s: &str) -> std::result::Result<CoprocessorMode, String> {
    match s.to_ascii_lowercase().as_str() {
        "auto" => Ok(CoprocessorMode::Auto),
        "lle" => Ok(CoprocessorMode::Lle),
        "hle" => Ok(CoprocessorMode::Hle),
        other => Err(format!(
            "unknown coprocessor mode '{other}' (expected auto|lle|hle)"
        )),
    }
}

impl CoprocessorArgs {
    /// Stage these flags on `emu` before `load_rom`: read the firmware
    /// file (if any) and set the emulation mode.
    pub fn apply(&self, emu: &mut SnesEmulator) -> Result<()> {
        if !snes1_core::NEC_LLE_BUILT {
            if self.coprocessor_mode == CoprocessorMode::Lle {
                anyhow::bail!(
                    "--coprocessor-mode lle: low-level emulation is not built into this \
                     snes1-cli (build it with `--features nec-lle`); DSP-1 games run on the HLE"
                );
            }
            if self.dsp1_firmware.is_some() {
                eprintln!(
                    "warning: --dsp1-firmware ignored: low-level emulation is not built into \
                     this snes1-cli (build it with `--features nec-lle`); the DSP-1 runs on the HLE"
                );
            }
        }
        emu.set_coprocessor_mode(self.coprocessor_mode);
        if let Some(path) = &self.dsp1_firmware {
            let fw = std::fs::read(path)
                .with_context(|| format!("failed to read DSP-1 firmware: {}", path.display()))?;
            emu.set_coprocessor_firmware(Some(fw));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_cli::trace::line::{FieldValue, TraceLine};
    use snes1_core::{
        format_trace_line_65816, format_trace_line_spc700, TraceEntryBinary65816,
        TraceEntryBinarySpc700,
    };

    fn number(line: &TraceLine, name: &str) -> Option<u64> {
        match line.field(name)?.value {
            FieldValue::Number(n) => Some(n),
            _ => None,
        }
    }

    #[test]
    fn the_command_line_is_well_formed_and_lists_every_command_in_order() {
        let cmd = retro_cli::command::<Snes>();
        cmd.clone().debug_assert();
        let mut listed: Vec<&str> = cmd
            .get_subcommands()
            .map(|s| s.get_name())
            .filter(|name| *name != "help")
            .collect();
        listed.sort_unstable();
        let mut ordered = Snes::COMMAND_ORDER.to_vec();
        ordered.sort_unstable();
        assert_eq!(listed, ordered);
    }

    #[test]
    fn the_rom_comes_first_where_a_command_takes_two_arguments() {
        let usage = |name: &str| {
            retro_cli::command::<Snes>()
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
        let cmd = retro_cli::command::<Snes>();
        let sub = cmd.find_subcommand(command).expect(command);
        let arg = sub.get_arguments().find(|a| a.get_id() == id).expect(id);
        arg.get_help().map(|h| h.to_string()).unwrap_or_default()
    }

    #[test]
    fn the_help_carries_the_notes_the_tool_gave() {
        assert_eq!(
            help_of("compare-trace", "cpu"),
            "Which CPU to compare: `snes` (65C816), `spc700`, `gsu` (Super FX) or `cx4` (Cx4 \
             HG51B169). `gsu` and `cx4` are compared with a log saved by this tool's own trace, \
             since bsnes-plus traces neither"
        );
        assert!(help_of("compare-trace", "skip_timing").contains("(the 65C816's V/H/F)"));
        assert!(help_of("compare-trace", "ignore").ends_with("(e.g. `A,X,DB,P,eff`)"));
        assert!(help_of("compare-trace", "press_at").contains("the run gets no input"));
        assert!(help_of("trace", "press")
            .contains("a screen waiting for a new press (a title, a menu, such as a Mode 5 menu)"));
        assert!(help_of("screenshot", "reset_at").contains(
            "a plain run cannot, most notably the SPC7110 A/B self-test (press A -> reset -> \
             press B -> reset -> the game boots), letting a single run reach real gameplay"
        ));
        assert!(help_of("screenshot", "show_only")
            .contains("(`0`=BG1, `1`=BG2, `2`=BG3, `3`=BG4 and `4`=OBJ)"));
        assert!(help_of("screenshot", "hide").contains("E.g. `--hide 0` drops BG1"));
        assert!(help_of("screenshot", "render_layers")
            .contains("(`<stem>_bg1`, `<stem>_bg2`, `<stem>_bg3`, `<stem>_bg4` and `<stem>_obj`)"));
        assert!(help_of("screenshot", "overscan").starts_with("Show the full overscan frame"));
        assert!(help_of("screenshot", "coprocessor_state")
            .contains("(e.g. S-DD1 `DECOMPS > 0` + DMA shadow)"));
        assert!(help_of("trace", "press_hold").contains("to run a routine while a button"));
        assert_eq!(
            help_of("trace-to-text", "rom"),
            "The ROM the trace was made from: the GSU's and the Cx4's lines need it, to know \
             which CPU the cartridge put in their slot; the 65C816's and the SPC700's need none"
        );
        assert!(help_of("info", "frames").ends_with("e.g. S-DD1 `DECOMPS > 0`"));
        assert!(help_of("run", "until_vblank").ends_with("the NMI not yet raised"));
        assert_eq!(
            help_of("run", "until_pc"),
            "Step until the 65C816 reaches this 24-bit PC (hex, e.g. `00ff03`)"
        );
        assert_eq!(
            help_of("disasm", "frames"),
            "Run this many frames before decoding (so PC + flags settle)"
        );
        assert_eq!(
            help_of("memory", "addr"),
            "Start address (24-bit hex, e.g. `7e0000`, `0x7E0000`, `$7E0000`)"
        );
        assert_eq!(
            help_of("fingerprint", "addr"),
            "Region start address (24-bit hex, default `7E0000` = Work RAM)"
        );
    }

    #[test]
    fn the_cpu_table_names_each_cpu_once_in_the_slot_the_core_gives_it() {
        let emu = SnesEmulator::new();
        let cpus = &emu.system_info().cpus;
        let mut names: Vec<&str> = Vec::new();
        for cpu in Snes::CPUS {
            // The cartridge's slot exists only once a cartridge brings a CPU.
            if let Some(info) = cpus.get(cpu.id) {
                assert_eq!(info.architecture, cpu.architecture, "{}", cpu.label);
                assert_eq!(info.address_width, cpu.address_bits, "{}", cpu.label);
            } else {
                assert_eq!(cpu.id, CARTRIDGE_CPU, "{}", cpu.label);
            }
            names.extend(cpu.names.iter().copied());
        }
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "a --cpu name is given twice");
    }

    #[test]
    fn a_cartridge_cpu_is_refused_on_a_machine_whose_slot_lacks_it() {
        let emu = SnesEmulator::new();
        let gsu = retro_cli::console::find_cpu::<Snes>("superfx").unwrap();
        let err = gsu.check_slot::<Snes>(&emu).unwrap_err();
        assert_eq!(err.to_string(), "--cpu gsu: this cartridge has no GSU");
        assert!(Snes::CPUS[0].check_slot::<Snes>(&emu).is_ok());
    }

    #[test]
    fn the_65c816_parser_reads_our_own_line_back() {
        let entry = TraceEntryBinary65816 {
            pbr_pc: 0x00_8001,
            a: 0x0280,
            x: 0x000a,
            y: 0x0001,
            s: 0x01f1,
            vcounter: 12,
            hdot: 50,
            ..Default::default()
        };
        let line = format_trace_line_65816(&entry);
        let parsed = (Snes::CPUS[0].parse_line)(&line).expect("parses");
        assert_eq!(parsed.pc, 0x00_8001);
        assert_eq!(number(&parsed, "A"), Some(0x0280));
        assert_eq!(number(&parsed, "X"), Some(0x000a));
        assert_eq!(number(&parsed, "S"), Some(0x01f1));
        assert_eq!(parsed.field("V").unwrap().value, FieldValue::Number(12));
        assert_eq!(parsed.field("H").unwrap().value, FieldValue::Number(50));
    }

    #[test]
    fn the_spc700_parser_reads_our_own_line_back() {
        let entry = TraceEntryBinarySpc700 {
            pc: 0xffc0,
            a: 0x12,
            x: 0x34,
            sp: 0xef,
            bytes_len: 2,
            bytes: [0xcd, 0xef, 0],
            ..Default::default()
        };
        let line = format_trace_line_spc700(&entry);
        let parsed = (Snes::CPUS[1].parse_line)(&line).expect("parses");
        assert_eq!(parsed.pc, 0xffc0);
        assert_eq!(number(&parsed, "A"), Some(0x12));
        assert_eq!(number(&parsed, "X"), Some(0x34));
    }

    #[cfg(feature = "chip-superfx")]
    #[test]
    fn the_gsu_parser_reads_our_own_line_back() {
        use snes1_core::{format_trace_line_gsu, TraceEntryBinaryGsu};
        let mut r = [0u16; 16];
        r[1] = 0xbeef;
        r[15] = 0x0104;
        let entry = TraceEntryBinaryGsu {
            pbr_pc: 0x01_8000,
            sfr: 0x0120,
            bytes_len: 1,
            bytes: [0x01, 0, 0],
            r,
            ..Default::default()
        };
        let line = format_trace_line_gsu(&entry);
        let parsed = (Snes::CPUS[2].parse_line)(&line).expect("parses");
        assert_eq!(parsed.pc, 0x01_8000);
        assert_eq!(number(&parsed, "R1"), Some(0xbeef));
        assert_eq!(number(&parsed, "R15"), Some(0x0104));
        assert_eq!(number(&parsed, "SFR"), Some(0x0120));
    }

    #[cfg(feature = "chip-cx4")]
    #[test]
    fn the_cx4_parser_reads_our_own_line_back() {
        use snes1_core::{format_trace_line_cx4, TraceEntryBinaryCx4};
        let mut gpr = [0u32; 16];
        gpr[1] = 0xab;
        let entry = TraceEntryBinaryCx4 {
            pc: 0x10,
            a: 0x12,
            p: 1,
            gpr,
            ..Default::default()
        };
        let line = format_trace_line_cx4(&entry);
        let parsed = (Snes::CPUS[3].parse_line)(&line).expect("parses");
        assert_eq!(parsed.pc, 0x10);
        assert_eq!(number(&parsed, "A"), Some(0x12));
        assert_eq!(number(&parsed, "P"), Some(1));
        assert_eq!(number(&parsed, "R1"), Some(0xab));
    }

    #[test]
    fn a_forced_mapping_is_refused_beside_a_manifest() {
        let boot = SnesBoot {
            force_detect: true,
            ..SnesBoot::default()
        };
        let mut emu = SnesEmulator::new();
        let err = Snes::stage(&mut emu, &[], &boot, true).unwrap_err();
        assert!(err.to_string().contains("both choose the mapping"), "{err}");
        assert!(Snes::stage(&mut emu, &[], &boot, false).is_ok());
        assert_eq!(Snes::golden_flags(&boot), vec![FORCE_DETECT]);
        assert!(Snes::golden_flags(&SnesBoot::default()).is_empty());
    }

    #[test]
    fn forced_lle_is_refused_by_a_build_without_it() {
        let args = CoprocessorArgs {
            dsp1_firmware: None,
            coprocessor_mode: CoprocessorMode::Lle,
        };
        let result = args.apply(&mut SnesEmulator::new());
        assert_eq!(result.is_err(), !snes1_core::NEC_LLE_BUILT);
        if let Err(err) = result {
            assert!(err.to_string().contains("not built"), "{err}");
        }
    }
}
