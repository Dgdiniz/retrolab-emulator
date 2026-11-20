//! What a console's command-line tool gives the shared commands.

use std::fmt::Debug;

use anyhow::Result;
use retro_core::golden::GoldenConsole;
use retro_core::{CpuArchitecture, EmulatorCore};

use crate::trace::line::TraceLine;

/// A console's command-line tool, implemented by the tool on a marker type
/// of its own. The shared commands read the console's facts from it, and
/// call it where the console does something its own way.
pub trait Console: Sized + 'static {
    /// The machine the tool drives.
    type Emulator: GoldenConsole;
    /// The flags every command of this tool that loads a ROM takes.
    type BootArgs: clap::Args + Clone + Debug + Default;
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

    /// Set the boot flags on a new machine, before its ROM loads.
    fn stage(emu: &mut Self::Emulator, rom: &[u8], boot: &Self::BootArgs) -> Result<()>;

    /// Set what the boot flags change once the ROM loaded.
    fn after_load(emu: &mut Self::Emulator, boot: &Self::BootArgs) -> Result<()> {
        let _ = (emu, boot);
        Ok(())
    }

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

    /// Run one of the tool's own commands.
    fn run_command(command: Self::Command) -> Result<()>;
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
    fn a_cpu_this_build_lacks_is_refused_with_its_reason() {
        let mut gsu = cpu(&["gsu", "superfx"], "GSU");
        assert!(gsu.check_available().is_ok());
        gsu.unavailable = Some("not built");
        let err = gsu.check_available().unwrap_err().to_string();
        assert_eq!(err, "--cpu gsu: not built");
    }
}
