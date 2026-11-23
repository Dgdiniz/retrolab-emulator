//! What a console's command-line tool gives the shared commands.

use std::fmt::Debug;
use std::io::{self, Write};
use std::path::Path;

use anyhow::Result;
use retro_core::audio::SoundCore;
use retro_core::capture::CaptureCore;
use retro_core::debug::ByteOrder;
use retro_core::frame::DebugMachine;
use retro_core::golden::GoldenConsole;
use retro_core::video::PixelImage;
use retro_core::{CpuArchitecture, EmulatorCore};

use crate::trace::line::TraceLine;

/// A console's command-line tool, implemented by the tool on a marker type
/// of its own. The shared commands read the console's facts from it, and
/// call it where the console does something its own way.
pub trait Console: Sized + 'static {
    /// The machine the tool drives.
    type Emulator: GoldenConsole + DebugMachine + CaptureCore + SoundCore;
    /// The flags every command of this tool that loads a ROM takes.
    type BootArgs: clap::Args + Clone + Debug + Default;
    /// The flags only this console's `screenshot` takes.
    type ScreenshotArgs: clap::Args + Clone + Debug + Default;
    /// The tool's own commands.
    type Command: clap::Subcommand + Debug;

    /// The tool's name, as `--version`, `version` and the usage lines print it.
    const TOOL: &'static str;
    /// The line the tool's help starts with.
    const ABOUT: &'static str;
    /// The tool's version.
    const VERSION: &'static str;
    /// The ROM files the tool loads, as its help names them.
    const ROM_FILES: &'static str;
    /// Field names the help of `--ignore` gives as an example.
    const IGNORE_EXAMPLE: &'static str;
    /// The emulator whose trace text the tool writes and compares with.
    const TRACE_FORMAT: &'static str;
    /// The help of `trace --timing`.
    const TIMING_HELP: &'static str;
    /// The CPUs `--cpu` names; the first is the main CPU.
    const CPUS: &'static [CliCpu];
    /// The tool's commands, shared and its own, in the order its help
    /// lists them.
    const COMMAND_ORDER: &'static [&'static str];
    /// The main CPU's work RAM on its bus: its first address and its size
    /// in bytes. `fingerprint` hashes it unless told otherwise,
    /// `determinism-check` says whether it differs where two machines part,
    /// and the help of `memory` gives its first address as an example.
    const WORK_RAM: (u32, u32);
    /// A coprocessor at work, as `info --frames` shows it: the example its
    /// help gives.
    const CHIP_ACTIVITY: &'static str;
    /// The interrupt the start of vertical blanking raises, as the help of
    /// `run --until-vblank` names it.
    const VBLANK_INTERRUPT: &'static str;
    /// An address of the main CPU's program, in hex, as wide as its
    /// addresses: the example the help of `run --until-pc` gives.
    const PC_EXAMPLE: &'static str;
    /// What the disassembly of the console's CPUs reads besides the
    /// program counter (`"flags"`), which `disasm --frames` lets settle;
    /// empty for nothing.
    const DECODED_WITH: &'static str;
    /// An address in work RAM: the example the help of `watch-ram --addr`
    /// gives.
    const WATCH_EXAMPLE: u32;
    /// How many instructions of the main CPU a frame runs, as the help of
    /// `watch-ram --max-steps` says it.
    const FRAME_INSTRUCTIONS: &'static str;
    /// The chip that draws the picture, as the help of `screenshot` names
    /// what it saves.
    const VIDEO_CHIP: &'static str;
    /// The chips that make the sound, as the help of `audio-dump` names
    /// what it saves.
    const SOUND_CHIPS: &'static str;
    /// What the help of `screenshot --render-layers` says of the files it
    /// writes besides one per layer (images of the console's own, how a
    /// layer shares the screen with another); empty for nothing.
    const LAYER_NOTES: &'static str;
    /// The help of `screenshot --overscan`.
    const OVERSCAN_HELP: &'static str;
    /// A coprocessor's state at work, as the help of `screenshot
    /// --coprocessor-state` gives its example.
    const CHIP_STATE_EXAMPLE: &'static str;
    /// The lines of the `profile` report, each a part of the machine the
    /// core names in its profiles.
    const PROFILE: &'static [ProfileLine];
    /// What the machine's clock counts, as the `profile` report names a
    /// count of it (`FrameProfile::elapsed`).
    const PROFILE_CLOCK: &'static str;
    /// What the help of the input flags adds after the console's buttons
    /// (buttons that reach a game only on another pad); empty for nothing.
    const BUTTONS_NOTE: &'static str;
    /// A screen of the console's games that waits for a new press, as the
    /// help of `--press` gives its example; empty for none.
    const PRESS_EXAMPLE: &'static str;
    /// A first boot of the console's games that only a reset gets past, as
    /// the help of `--reset-at` gives its example; empty for none.
    const RESET_EXAMPLE: &'static str;

    /// Set the boot flags on a new machine, before its ROM loads.
    /// `manifest` says whether the command stages a cartridge manifest too
    /// (`--manifest`, or the one `manifest validate` checks), which a flag
    /// that chooses the mapping itself cannot go with.
    fn stage(
        emu: &mut Self::Emulator,
        rom: &[u8],
        boot: &Self::BootArgs,
        manifest: bool,
    ) -> Result<()>;

    /// Set what the boot flags change once the ROM loaded.
    fn after_load(emu: &mut Self::Emulator, boot: &Self::BootArgs) -> Result<()> {
        let _ = (emu, boot);
        Ok(())
    }

    /// The flags of the console's golden manifests ([`GoldenConsole::FLAGS`])
    /// the boot flags amount to, which `golden` writes in its record's
    /// header.
    fn golden_flags(boot: &Self::BootArgs) -> Vec<&'static str>;

    /// What `info` prints about the ROM file at `path` and its header,
    /// before the chips and the memory map.
    fn write_rom_header(
        emu: &Self::Emulator,
        rom: &[u8],
        path: &Path,
        out: &mut dyn Write,
    ) -> io::Result<()>;

    /// The text of trace entries of `cpu` the debugger handed out, one line
    /// per entry, each followed by `"\n"`. `timing` asks for the timing
    /// columns a console's lines carry only on request. By default the
    /// core's text ([`core_trace_text`]).
    fn trace_text(
        emu: &mut Self::Emulator,
        cpu: &CliCpu,
        data: &[u8],
        timing: bool,
    ) -> Result<String> {
        let _ = timing;
        core_trace_text(emu, cpu, data)
    }

    /// The text of a saved trace of `cpu` when no ROM is given, for a CPU
    /// whose lines disassemble its memory: what the lines can show without
    /// it. `None`, the default, makes the text on a machine with no ROM.
    fn trace_text_without_rom(cpu: &CliCpu, data: &[u8]) -> Option<String> {
        let _ = (cpu, data);
        None
    }

    /// `screenshot`: what the console's own flags set before the run's last
    /// frame.
    fn before_last_frame(emu: &mut Self::Emulator, args: &Self::ScreenshotArgs) -> Result<()> {
        let _ = (emu, args);
        Ok(())
    }

    /// `screenshot`: what the console's own flags do once the run's frames
    /// are done, before the picture is saved: a change to the machine that
    /// one more frame shows ([`crate::cmd::screenshot::draw_again`]), and
    /// undoing what [`Console::before_last_frame`] set.
    fn after_frames(emu: &mut Self::Emulator, args: &Self::ScreenshotArgs) -> Result<()> {
        let _ = (emu, args);
        Ok(())
    }

    /// `screenshot`: warnings about the picture the machine shows after
    /// `frames` frames, such as a screen the console has blanked, each
    /// printed as `warning: ...` before the picture is saved.
    fn picture_notes(emu: &Self::Emulator, frames: u64) -> Vec<String> {
        let _ = (emu, frames);
        Vec::new()
    }

    /// `screenshot --render-layers`: the images of the held frame it writes
    /// after the layers, each with the suffix of its file.
    fn extra_layer_images(emu: &mut Self::Emulator) -> Vec<(&'static str, PixelImage)> {
        let _ = emu;
        Vec::new()
    }

    /// Run one of the tool's own commands.
    fn run_command(command: Self::Command) -> Result<()>;
}

/// A line of the `profile` report: the part of the machine whose time it
/// shows.
#[derive(Clone, Copy, Debug)]
pub struct ProfileLine {
    /// What the report calls it.
    pub label: &'static str,
    /// The name the core gives it in its profiles (`ChipTime::chip`); a
    /// CPU's is the label of the CPU in [`Console::CPUS`].
    pub chip: &'static str,
}

/// A CPU the trace commands can name with `--cpu`.
#[derive(Clone, Copy, Debug)]
pub struct CliCpu {
    /// The debugger's id of the CPU's slot.
    pub id: usize,
    /// What `--cpu` takes for it; the first is the one shown.
    pub names: &'static [&'static str],
    /// What reports call it.
    pub label: &'static str,
    /// What the help calls it, after its name; empty when the name says it.
    pub description: &'static str,
    /// The CPU its slot must hold: a cartridge's slot holds the CPU the
    /// cartridge brought, which may be another one.
    pub architecture: CpuArchitecture,
    /// How many bits its addresses have, as the help names them: the
    /// core's `CpuInfo::address_width` for the CPU.
    pub address_bits: u8,
    /// How many bytes the bytes column of its disassembly is as wide as.
    pub disassembly_bytes: usize,
    /// The order of the bytes of a wider value in its memory.
    pub byte_order: ByteOrder,
    /// The timing columns its lines and its reference's carry, as the
    /// comparison's header names them; empty for none.
    pub timing_columns: &'static str,
    /// Parse one of its trace lines, ours or the reference's.
    pub parse_line: fn(&str) -> Option<TraceLine>,
    /// No reference emulator traces it: `compare-trace` compares it with a
    /// log this tool's own `trace` saved.
    pub compared_with_own_trace: bool,
    /// Why this build of the tool cannot trace it, when it cannot.
    pub unavailable: Option<&'static str>,
}

impl CliCpu {
    /// The name `--cpu` shows for it.
    pub fn name(&self) -> &'static str {
        self.names[0]
    }

    /// Refuse a CPU this build cannot trace.
    pub fn check_available(&self) -> Result<()> {
        match self.unavailable {
            Some(why) => anyhow::bail!("--cpu {}: {why}", self.name()),
            None => Ok(()),
        }
    }

    /// Refuse a CPU the machine's slot does not hold: a cartridge's slot
    /// holds the CPU the cartridge brought, or none.
    pub fn check_slot<C: Console>(&self, emu: &C::Emulator) -> Result<()> {
        match emu.system_info().cpus.get(self.id) {
            Some(info) if info.architecture == self.architecture => Ok(()),
            Some(info) => {
                let held = C::CPUS
                    .iter()
                    .find(|cpu| cpu.id == self.id && cpu.architecture == info.architecture)
                    .map_or(info.name.as_str(), |cpu| cpu.label);
                anyhow::bail!(
                    "--cpu {}: this cartridge has a {held} in that slot, not a {}",
                    self.name(),
                    self.label
                )
            }
            None => anyhow::bail!(
                "--cpu {}: this cartridge has no {}",
                self.name(),
                self.label
            ),
        }
    }
}

/// The CPU `--cpu` names, by any of its names, in any case.
pub fn find_cpu<C: Console>(name: &str) -> Result<&'static CliCpu> {
    let wanted = name.trim().to_ascii_lowercase();
    C::CPUS
        .iter()
        .find(|cpu| cpu.names.contains(&wanted.as_str()))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "unknown --cpu '{wanted}' (expected {})",
                cpu_choices(C::CPUS)
            )
        })
}

/// The CPUs' names as a sentence: `m68k or z80`, `snes, spc700, gsu or cx4`.
pub fn cpu_choices(cpus: &[CliCpu]) -> String {
    join_choices(cpus.iter().map(|cpu| cpu.name().to_string()).collect())
}

/// The CPUs as help names them: `` `m68k` (68000) or `z80` (Z80) ``.
pub fn cpu_help_list(cpus: &[CliCpu]) -> String {
    join_choices(
        cpus.iter()
            .map(|cpu| match cpu.description {
                "" => format!("`{}`", cpu.name()),
                description => format!("`{}` ({description})", cpu.name()),
            })
            .collect(),
    )
}

fn join_choices(items: Vec<String>) -> String {
    join_words(items, "or")
}

/// Items as a sentence: `a`, `a and b`, `a, b and c` (`word` the last link).
pub fn join_words(mut items: Vec<String>, word: &str) -> String {
    match items.len() {
        0 => String::new(),
        1 => items.remove(0),
        _ => {
            let last = items.pop().unwrap_or_default();
            format!("{} {word} {last}", items.join(", "))
        }
    }
}

/// The program counter of CPU `cpu`, the debugger's states refreshed
/// first: a CPU that runs in banks has its bank in it, and the value is
/// cut to the CPU's address width. 0 for a CPU the machine lacks.
pub fn program_counter<E: EmulatorCore>(emu: &mut E, cpu: usize) -> u32 {
    emu.refresh_debugger_state();
    held_program_counter(emu, cpu)
}

/// The program counter of CPU `cpu` from the debugger's states as they
/// were last refreshed, which a host step does after every step; read as
/// [`program_counter`] reads it, borrowing the state rather than copying
/// it, so a loop can read it after every step.
pub fn held_program_counter<E: EmulatorCore>(emu: &E, cpu: usize) -> u32 {
    let pc = emu
        .debugger_ref()
        .get_cpu_state(cpu)
        .map_or(0, |state| state.pc);
    let bits = emu
        .system_info()
        .cpus
        .get(cpu)
        .map_or(32, |info| info.address_width);
    pc & address_mask(bits)
}

/// The mask of an address `bits` wide.
pub fn address_mask(bits: u8) -> u32 {
    match bits {
        32.. => u32::MAX,
        bits => (1u32 << bits) - 1,
    }
}

/// The core's text of trace entries of `cpu` the debugger handed out.
pub fn core_trace_text<E: EmulatorCore>(emu: &mut E, cpu: &CliCpu, data: &[u8]) -> Result<String> {
    emu.trace_to_text(cpu.id, data).ok_or_else(|| {
        anyhow::anyhow!(
            "--cpu {}: the {} has no trace text on this machine (a cartridge's CPU needs the ROM \
             that brings it)",
            cpu.name(),
            cpu.label
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cpu(names: &'static [&'static str], label: &'static str) -> CliCpu {
        CliCpu {
            id: 0,
            names,
            label,
            description: label,
            architecture: CpuArchitecture::M68000,
            address_bits: 24,
            disassembly_bytes: 4,
            byte_order: ByteOrder::BigEndian,
            timing_columns: "",
            parse_line: |_| None,
            compared_with_own_trace: false,
            unavailable: None,
        }
    }

    #[test]
    fn the_choices_read_as_a_sentence() {
        let one = [cpu(&["m68k"], "68000")];
        let two = [cpu(&["m68k", "68k"], "68000"), cpu(&["z80"], "Z80")];
        let four = [
            cpu(&["snes"], "65C816"),
            cpu(&["spc700"], "SPC700"),
            cpu(&["gsu"], "GSU"),
            cpu(&["cx4"], "Cx4"),
        ];
        assert_eq!(cpu_choices(&one), "m68k");
        assert_eq!(cpu_choices(&two), "m68k or z80");
        assert_eq!(cpu_choices(&four), "snes, spc700, gsu or cx4");
        assert_eq!(cpu_help_list(&two), "`m68k` (68000) or `z80` (Z80)");
        let mut bare = two;
        bare[1].description = "";
        assert_eq!(cpu_help_list(&bare), "`m68k` (68000) or `z80`");
        let words = |n: usize| (0..n).map(|i| i.to_string()).collect::<Vec<_>>();
        assert_eq!(join_words(words(3), "and"), "0, 1 and 2");
        assert_eq!(join_words(words(1), "and"), "0");
    }

    #[test]
    fn an_address_mask_is_as_wide_as_its_bits() {
        assert_eq!(address_mask(16), 0xFFFF);
        assert_eq!(address_mask(24), 0x00FF_FFFF);
        assert_eq!(address_mask(32), u32::MAX);
        assert_eq!(address_mask(40), u32::MAX);
    }

    #[test]
    fn a_cpu_this_build_lacks_is_refused_with_its_reason() {
        let mut gsu = cpu(&["gsu", "superfx"], "GSU");
        assert!(gsu.check_available().is_ok());
        gsu.unavailable = Some("not built");
        let err = gsu.check_available().unwrap_err().to_string();
        assert_eq!(err, "--cpu gsu: not built");
    }
}
