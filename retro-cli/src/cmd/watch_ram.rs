//! `watch-ram`: step a ROM through a window an instruction at a time and
//! report every write that changes a watched cell of memory, naming the
//! instruction (its address and disassembly) that made it.
//!
//! Where a video-memory watch arms the video chip's hook, this works for
//! the CPU's RAM: it reads the program counter before each step and the
//! cell after it. It answers "who scheduled this task pointer?" or "what
//! scribbled this state variable?", where the address is reached through
//! an indirect write the disassembly alone would not reveal.
//!
//! To keep it cheap, the boot runs at full speed for `--warmup-frames`;
//! only the `--max-steps` window is stepped. `--report-pc` prints the
//! watched value (and the CPU's registers) whenever the program counter is
//! about to run a given address, which reproduces a known divergence (a
//! routine reading the cell) without scrolling the whole log of changes.

use anyhow::Result;
use retro_core::debug::{ByteOrder, MAIN_CPU};
use retro_core::{EmulatorCore, RegisterInfo, RegisterValue};

use crate::console::{address_mask, held_program_counter, program_counter, Console};
use crate::input::InputArgs;
use crate::parse::parse_hex_u32;
use crate::rom::{load, write_banner, RomArgs};
use crate::trace::STEP_CAP;

/// How many times `--report-pc` prints a hit; the rest are counted.
const REPORTS_SHOWN: u64 = 4;

#[derive(clap::Args, Debug)]
pub struct WatchRamArgs<B: clap::Args> {
    #[command(flatten)]
    pub rom: RomArgs<B>,
    /// Work-RAM address to watch (hex).
    #[arg(long, value_parser = parse_hex_u32)]
    pub addr: u32,
    /// Width of the watched cell in bytes (1, 2 or 4).
    #[arg(long, default_value_t = 4)]
    pub width: u32,
    /// Frames to run at full speed before the single-stepped window.
    #[arg(long, default_value_t = 0)]
    pub warmup_frames: u64,
    /// Instructions to single-step while watching the cell.
    #[arg(long, default_value_t = 200_000)]
    pub max_steps: u64,
    /// Stop at the first change instead of logging every change.
    #[arg(long)]
    pub until_change: bool,
    /// Also print the watched value every time this PC is about to
    /// execute (hex).
    #[arg(long, value_parser = parse_hex_u32)]
    pub report_pc: Option<u32>,
    #[command(flatten)]
    pub input: InputArgs,
}

pub fn run<C: Console>(args: WatchRamArgs<C::BootArgs>) -> Result<()> {
    if !matches!(args.width, 1 | 2 | 4) {
        anyhow::bail!("--width must be 1, 2 or 4");
    }
    let input = args.input.plan::<C::Emulator>()?;
    let main = &C::CPUS[0];
    let digits = (args.width * 2) as usize;
    let cell = |emu: &mut C::Emulator| read_cell(emu, args.addr, args.width, main.byte_order);

    let (_, mut emu) = load::<C>(&args.rom)?;
    write_banner(&mut emu, &mut std::io::stdout())?;

    for frame in 0..args.warmup_frames {
        input.before_frame(&mut emu, frame);
        emu.run_frame();
    }
    println!(
        "Warmed up {} frame(s); single-stepping up to {} instruction(s) watching ${:06X} ({}B)",
        args.warmup_frames, args.max_steps, args.addr, args.width
    );
    let mut prev = cell(&mut emu);
    println!("  initial ${:06X} = {prev:0digits$X}", args.addr);

    // A host step records its instruction in the history, which nothing
    // here reads: the steps keep none. A host step refreshes the
    // debugger's CPU states after it, so the program counter and the
    // registers are read afresh only before the first.
    emu.debugger().set_history_enabled(MAIN_CPU, false);
    let mask = address_mask(
        emu.system_info()
            .cpus
            .get(MAIN_CPU)
            .map_or(32, |cpu| cpu.address_width),
    );
    let report_pc = args.report_pc.map(|pc| pc & mask);
    let mut pc = program_counter(&mut emu, MAIN_CPU);
    let (mut steps, mut host_steps, mut changes, mut report_hits) = (0u64, 0u64, 0u64, 0u64);

    while steps < args.max_steps {
        if report_pc == Some(pc) {
            let value = cell(&mut emu);
            report_hits += 1;
            if report_hits <= REPORTS_SHOWN {
                println!(
                    "  [s={steps}] >>> PC={pc:06X}: ${:06X} = {value:0digits$X}   {}",
                    args.addr,
                    registers(&emu)
                );
            }
        }
        if host_steps >= STEP_CAP {
            anyhow::bail!(
                "{host_steps} steps ran only {steps} of {} instructions: the {} stopped, or waits \
                 for an interrupt that never comes",
                args.max_steps,
                main.label
            );
        }
        let before = pc;
        // A step that only waits, or only takes an interrupt, runs no
        // instruction.
        let executed = emu.step_cpu(MAIN_CPU).instruction_executed;
        host_steps += 1;
        pc = held_program_counter(&emu, MAIN_CPU);
        if !executed {
            continue;
        }
        steps += 1;

        let now = cell(&mut emu);
        if now != prev {
            changes += 1;
            println!(
                "  [s={steps}] PC={before:06X}  ${:06X}: {prev:0digits$X} -> {now:0digits$X}   | {}",
                args.addr,
                disassemble_one(&mut emu, before),
            );
            prev = now;
            if args.until_change {
                break;
            }
        }
    }

    println!(
        "Done: {changes} change(s), {report_hits} report-pc hit(s) over {steps} stepped \
         instruction(s){}",
        if steps >= args.max_steps {
            " (hit --max-steps cap)"
        } else {
            ""
        }
    );
    Ok(())
}

/// The watched cell: `width` bytes from `addr`, read without a side
/// effect, as one value in the CPU's byte order.
fn read_cell<E: EmulatorCore>(emu: &mut E, addr: u32, width: u32, order: ByteOrder) -> u32 {
    cell_value(&emu.read_memory_debug(addr, width), order)
}

/// `bytes`, the first at the lowest address, as one value in `order`.
fn cell_value(bytes: &[u8], order: ByteOrder) -> u32 {
    let fold = |value: u32, byte: &u8| (value << 8) | u32::from(*byte);
    match order {
        ByteOrder::BigEndian => bytes.iter().fold(0, fold),
        ByteOrder::LittleEndian => bytes.iter().rev().fold(0, fold),
    }
}

/// The main CPU's registers as the debugger holds them ([`register_list`]).
fn registers<E: EmulatorCore>(emu: &E) -> String {
    let values = emu
        .debugger_ref()
        .get_cpu_state(MAIN_CPU)
        .map(|state| state.registers.as_slice())
        .unwrap_or_default();
    let widths = emu
        .system_info()
        .cpus
        .get(MAIN_CPU)
        .map(|cpu| cpu.registers.as_slice())
        .unwrap_or_default();
    register_list(values, widths)
}

/// Registers as `NAME=VALUE`, each in hex as wide as `widths` says the
/// register is, 4 digits for one it does not name.
fn register_list(values: &[RegisterValue], widths: &[RegisterInfo]) -> String {
    values
        .iter()
        .map(|register| {
            let bits = widths
                .iter()
                .find(|info| info.name == register.name)
                .map_or(16, |info| usize::from(info.size_bits));
            let digits = bits.div_ceil(4);
            format!("{}={:0digits$X}", register.name, register.value)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The instruction at `pc`, as `MNEMONIC OPERANDS`.
fn disassemble_one<E: EmulatorCore>(emu: &mut E, pc: u32) -> String {
    match emu.disassemble_cpu(MAIN_CPU, pc, 1).first() {
        Some(insn) => format!("{} {}", insn.mnemonic, insn.operands),
        None => "??".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cell_reads_in_the_cpu_s_byte_order() {
        let bytes = [0x12, 0x34, 0x56, 0x78];
        assert_eq!(cell_value(&bytes, ByteOrder::BigEndian), 0x1234_5678);
        assert_eq!(cell_value(&bytes, ByteOrder::LittleEndian), 0x7856_3412);
        assert_eq!(cell_value(&bytes[..2], ByteOrder::LittleEndian), 0x3412);
        assert_eq!(cell_value(&[0xAA], ByteOrder::BigEndian), 0xAA);
    }

    #[test]
    fn registers_are_as_wide_as_the_core_says_and_4_digits_otherwise() {
        use retro_core::{DisplayFormat, RegisterCategory};
        let value = |name: &str, value: u64| RegisterValue {
            name: name.to_string(),
            value,
        };
        let width = |name: &str, size_bits: u8| RegisterInfo {
            name: name.to_string(),
            size_bits,
            display_format: DisplayFormat::Hexadecimal,
            category: RegisterCategory::General,
        };
        let values = [value("D0", 0x7208), value("SR", 0x2700), value("DB", 0x7E)];
        let widths = [width("D0", 32), width("SR", 16)];
        assert_eq!(
            register_list(&values, &widths),
            "D0=00007208 SR=2700 DB=007E"
        );
        assert_eq!(register_list(&[], &widths), "");
    }
}
