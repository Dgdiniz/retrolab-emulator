//! `watch-read`: single-step a ROM through a window of instructions and
//! report every 68000 read that lands in one or more address ranges,
//! naming the instruction (PC + disassembly) that made it.
//!
//! This is the read-side companion of `watch-ram`. It answers "which
//! routine reads this table / text block / font?" for data the CPU only
//! ever *reads*, such as anything in ROM, where a write watch can never
//! fire and the address is usually reached through an indirect `(An)` read
//! the disassembly alone would not reveal.
//!
//! It turns on the core's instruction access log, which keeps the bus
//! accesses of each stepped instruction under that instruction's own
//! address. An interrupt the step enters before the instruction and the
//! Z80 are not in the log. A VDP DMA that copies from a watched range (a
//! font sent to VRAM, say) is reported too, under the instruction whose
//! write to the control port started it, with where the words went: the
//! VDP reads its source itself, so it is not among the 68000's reads. The
//! bus does not tell a fetch from a data read, so the reads inside the
//! instruction's own bytes (its opcode and extension words) are left out
//! unless `--include-fetch` is given.
//!
//! As in `watch-ram`, the boot/attract prologue runs at full frame speed
//! (`--warmup-frames`, with `--press-at` applied per frame) and only the
//! `--max-steps` window is single-stepped.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use retro_core::{DebuggerCore, EmulatorCore, ExecutionMode};
use sg1_core::system::InstructionAccessLog;
use sg1_core::vdp::dma::{DmaTarget, MemoryDma};
use sg1_core::GenesisEmulator;

use super::{apply_press, connect_controllers, open_output, parse_press_schedule};

#[derive(clap::Args, Debug)]
pub struct WatchReadArgs {
    /// Path to the ROM file (`.bin` / `.md` / `.gen`).
    pub rom: PathBuf,
    /// Address range to watch, `START-END` inclusive or a single address
    /// (24-bit hex, e.g. `D6EBA-DC0BF`). Repeat the flag to watch several
    /// ranges at once.
    #[arg(short, long = "range", required = true)]
    pub ranges: Vec<String>,
    /// Frames to run at full speed before the single-stepped window.
    #[arg(long, default_value_t = 0)]
    pub warmup_frames: u64,
    /// Instructions to single-step while watching (a Genesis frame is
    /// roughly 60k-130k 68000 instructions).
    #[arg(long, default_value_t = 2_000_000)]
    pub max_steps: u64,
    /// Stop after this many matching reads and DMA transfers (0 = no limit).
    #[arg(long, default_value_t = 0)]
    pub max_hits: u64,
    /// Print only the per-PC summary, not every read and transfer.
    #[arg(long)]
    pub summary_only: bool,
    /// Also report the reads of the instruction's own opcode and extension
    /// words, which are fetches rather than data reads.
    #[arg(long)]
    pub include_fetch: bool,
    /// Connect 6-button pads so `x`/`y`/`z`/`mode` reach the game.
    #[arg(long)]
    pub six_button: bool,
    /// Frame-scheduled player-1 input during the warmup, same format as
    /// `screenshot`: `"F:buttons;F:buttons"` (e.g. `"180:start;186:"`).
    #[arg(long)]
    pub press_at: Option<String>,
    /// Write the report to a file instead of stdout.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
}

/// One inclusive watched address window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WatchRange {
    start: u32,
    end: u32,
}

impl WatchRange {
    /// Whether any byte of the access `[addr, addr + size)` is watched.
    fn overlaps(&self, addr: u32, size: u8) -> bool {
        self.overlaps_span(addr, addr.wrapping_add(u32::from(size.max(1)) - 1))
    }

    /// Whether any byte of the inclusive span `[lo, hi]` is watched.
    fn overlaps_span(&self, lo: u32, hi: u32) -> bool {
        lo <= self.end && hi >= self.start
    }
}

/// Whether a hit is the 68000 reading the range or a DMA copying from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum HitKind {
    Read,
    Dma,
}

/// An instruction the report names: its address and the opcode word it
/// fetched first. On the 68000 the opcode word alone sets an instruction's
/// length, and two instructions that ran at one address (code in RAM, a
/// bank-switched cartridge) differ in it.
type InstructionKey = (u32, Option<u16>);

/// An instruction's disassembly text and length in bytes.
#[derive(Debug, Clone)]
struct Disassembly {
    text: String,
    len: u32,
}

/// Per-PC aggregate for the closing summary.
#[derive(Debug)]
struct PcStats {
    hits: u64,
    lo: u32,
    hi: u32,
}

/// What the run found: how many reads and transfers, and per instruction
/// and kind of hit, how many and the span of addresses they covered.
#[derive(Debug, Default)]
struct Findings {
    reads: u64,
    dmas: u64,
    stats: HashMap<(InstructionKey, HitKind), PcStats>,
}

impl Findings {
    fn record(&mut self, key: InstructionKey, kind: HitKind, lo: u32, hi: u32) {
        match kind {
            HitKind::Read => self.reads += 1,
            HitKind::Dma => self.dmas += 1,
        }
        let entry = self
            .stats
            .entry((key, kind))
            .or_insert(PcStats { hits: 0, lo, hi });
        entry.hits += 1;
        entry.lo = entry.lo.min(lo);
        entry.hi = entry.hi.max(hi);
    }

    fn total(&self) -> u64 {
        self.reads + self.dmas
    }

    fn distinct_pcs(&self) -> usize {
        let mut pcs: Vec<u32> = self.stats.keys().map(|((pc, _), _)| *pc).collect();
        pcs.sort_unstable();
        pcs.dedup();
        pcs.len()
    }
}

pub fn run(args: WatchReadArgs) -> Result<()> {
    let ranges = args
        .ranges
        .iter()
        .map(|s| parse_range(s))
        .collect::<Result<Vec<_>>>()?;
    let schedule = parse_press_schedule(args.press_at.as_deref())?;
    let mut out = open_output(args.output.as_ref())?;

    let (_, mut emu) = super::load_rom(&args.rom)?;
    super::print_rom_title(&emu);
    if args.six_button {
        connect_controllers(&mut emu, true);
    }

    // Boot/attract prologue at full frame speed.
    emu.debugger().set_execution_mode(ExecutionMode::Running);
    for frame in 0..args.warmup_frames {
        apply_press(&mut emu, schedule.as_ref(), frame);
        emu.run_frame();
    }
    writeln!(
        out,
        "Warmed up {} frame(s); single-stepping up to {} instruction(s) watching reads and DMA transfers in {}",
        args.warmup_frames,
        args.max_steps,
        describe_ranges(&ranges)
    )?;

    // StepInto does not tick the frame counter, so the window is bounded by
    // stepped instructions; the warmup already positioned the run. A step
    // that executes nothing (a stopped CPU waiting for an interrupt) still
    // advances time, so the loop is also capped on raw iterations.
    emu.debugger().set_execution_mode(ExecutionMode::StepInto);
    emu.set_instruction_access_logging(true);

    let max_iterations = args.max_steps.saturating_mul(64).max(1_000_000);
    let mut iterations = 0u64;
    let mut steps = 0u64;
    let mut found = Findings::default();
    let mut disassembly: HashMap<InstructionKey, Disassembly> = HashMap::new();
    let reached_max_hits = |found: &Findings| args.max_hits != 0 && found.total() >= args.max_hits;

    'stepping: while steps < args.max_steps && iterations < max_iterations {
        iterations += 1;
        if !emu.step().instruction_executed {
            continue;
        }
        steps += 1;
        let log = emu.take_instruction_access_log();
        let key = (log.pc, opcode_of(&log));

        for &(addr, size, is_write, value) in &log.accesses {
            if is_write || !ranges.iter().any(|w| w.overlaps(addr, size)) {
                continue;
            }
            let instr = disassemble(&mut emu, &mut disassembly, key);
            if !args.include_fetch && is_own_fetch(addr, log.pc, instr.len) {
                continue;
            }
            let last = addr.wrapping_add(u32::from(size.max(1)) - 1);
            found.record(key, HitKind::Read, addr, last);
            if !args.summary_only {
                writeln!(
                    out,
                    "  [s={steps}] PC={:06X}  {}   | {}",
                    log.pc,
                    format_read(addr, size, value),
                    instr.text
                )?;
            }
            if reached_max_hits(&found) {
                break 'stepping;
            }
        }

        for dma in &log.dmas {
            let spans = dma.source_spans();
            let watched = spans
                .iter()
                .any(|&(lo, hi)| ranges.iter().any(|w| w.overlaps_span(lo, hi)));
            if !watched {
                continue;
            }
            let instr = disassemble(&mut emu, &mut disassembly, key);
            let lo = spans.iter().map(|&(lo, _)| lo).min().unwrap_or(dma.source);
            let hi = spans.iter().map(|&(_, hi)| hi).max().unwrap_or(dma.source);
            found.record(key, HitKind::Dma, lo, hi);
            if !args.summary_only {
                writeln!(
                    out,
                    "  [s={steps}] PC={:06X}  {}   | {}",
                    log.pc,
                    format_dma(dma),
                    instr.text
                )?;
            }
            if reached_max_hits(&found) {
                break 'stepping;
            }
        }
    }
    emu.set_instruction_access_logging(false);

    let cap_note = if reached_max_hits(&found) {
        " (hit --max-hits cap)"
    } else if steps >= args.max_steps {
        " (hit --max-steps cap)"
    } else if iterations >= max_iterations {
        " (CPU idle: hit the iteration cap)"
    } else {
        ""
    };
    writeln!(
        out,
        "Done: {} read(s) and {} DMA transfer(s) in range over {steps} stepped instruction(s){cap_note}; {} distinct PC(s)",
        found.reads,
        found.dmas,
        found.distinct_pcs()
    )?;

    let mut rows: Vec<(&(InstructionKey, HitKind), &PcStats)> = found.stats.iter().collect();
    rows.sort_by(|a, b| b.1.hits.cmp(&a.1.hits).then(a.0.cmp(b.0)));
    for ((key, kind), s) in rows {
        let text = disassembly
            .get(key)
            .map(|d| d.text.as_str())
            .unwrap_or("??");
        let label = match kind {
            HitKind::Read => "reads",
            HitKind::Dma => "DMAs",
        };
        writeln!(
            out,
            "  PC={:06X}  {label:<5} {:>8}  ${:06X}-${:06X}   | {text}",
            key.0, s.hits, s.lo, s.hi
        )?;
    }
    out.flush()?;
    Ok(())
}

/// Parse a `START-END` (inclusive) or single-address watch range. Each side
/// is 24-bit hex and may carry a `0x`, `0X` or `$` prefix.
fn parse_range(spec: &str) -> Result<WatchRange> {
    let parse = |s: &str| -> Result<u32> {
        let cleaned = s
            .trim()
            .trim_start_matches("0x")
            .trim_start_matches("0X")
            .trim_start_matches('$');
        let v = u32::from_str_radix(cleaned, 16)
            .with_context(|| format!("invalid address '{}' in --range", s.trim()))?;
        if v > 0x00FF_FFFF {
            anyhow::bail!("address '{}' in --range is wider than 24 bits", s.trim());
        }
        Ok(v)
    };
    let (start, end) = match spec.split_once('-') {
        Some((a, b)) => (parse(a)?, parse(b)?),
        None => {
            let a = parse(spec)?;
            (a, a)
        }
    };
    if start > end {
        anyhow::bail!("--range '{spec}' ends before it starts");
    }
    Ok(WatchRange { start, end })
}

/// `$START-$END` list for the header line.
fn describe_ranges(ranges: &[WatchRange]) -> String {
    ranges
        .iter()
        .map(|r| format!("${:06X}-${:06X}", r.start, r.end))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The opcode word the logged instruction fetched: the first access in the
/// log is the fetch of the opcode at the instruction's own address.
fn opcode_of(log: &InstructionAccessLog) -> Option<u16> {
    match log.accesses.first() {
        Some(&(addr, 2, false, opcode)) if addr == log.pc => Some(opcode),
        _ => None,
    }
}

/// Whether a read at `addr` is the instruction at `pc` fetching its own
/// opcode or extension words: the core fetches exactly the instruction's
/// `len` bytes and nothing ahead of them.
fn is_own_fetch(addr: u32, pc: u32, len: u32) -> bool {
    addr >= pc && addr < pc.wrapping_add(len)
}

/// `$ADDR.b = $VV 'c'` / `$ADDR.w = $VVVV 'cc'`, with the printable
/// ASCII rendering that makes text reads easy to spot.
fn format_read(addr: u32, size: u8, value: u16) -> String {
    let show = |b: u8| -> char {
        if (0x20..0x7F).contains(&b) {
            b as char
        } else {
            '.'
        }
    };
    if size == 2 {
        let (hi, lo) = ((value >> 8) as u8, (value & 0xFF) as u8);
        format!("${addr:06X}.w = ${value:04X} '{}{}'", show(hi), show(lo))
    } else {
        let b = (value & 0xFF) as u8;
        format!("${addr:06X}.b = ${b:02X}   '{}'", show(b))
    }
}

/// `DMA $SRC-$END -> VRAM $DEST, N words`: a transfer from 68000 memory,
/// its source as the spans it read (two when it wrapped inside its 128 KB
/// window) and where it wrote.
fn format_dma(dma: &MemoryDma) -> String {
    let spans = dma
        .source_spans()
        .iter()
        .map(|&(lo, hi)| format!("${lo:06X}-${hi:06X}"))
        .collect::<Vec<_>>()
        .join(" + ");
    let target = match dma.target {
        DmaTarget::Vram => "VRAM",
        DmaTarget::Cram => "CRAM",
        DmaTarget::Vsram => "VSRAM",
        DmaTarget::Nowhere => "no memory",
    };
    format!(
        "DMA {spans} -> {target} ${:04X}, {} words",
        dma.destination, dma.words
    )
}

/// Disassemble the instruction `key` names once and remember it for the
/// summary. The key carries the opcode, so another instruction run later at
/// the same address is disassembled again.
fn disassemble<'a>(
    emu: &mut GenesisEmulator,
    cache: &'a mut HashMap<InstructionKey, Disassembly>,
    key: InstructionKey,
) -> &'a Disassembly {
    cache.entry(key).or_insert_with(|| {
        let instrs = emu.disassemble_at(key.0, 1);
        match instrs.first() {
            Some(i) => Disassembly {
                text: format!("{} {}", i.mnemonic, i.operands).trim().to_string(),
                len: (i.bytes.len() as u32).max(2),
            },
            None => Disassembly {
                text: "??".to_string(),
                len: 2,
            },
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ranges_and_single_addresses() {
        assert_eq!(
            parse_range("D6EBA-DC0BF").unwrap(),
            WatchRange {
                start: 0x0D_6EBA,
                end: 0x0D_C0BF
            }
        );
        assert_eq!(
            parse_range("0x200-$3FF").unwrap(),
            WatchRange {
                start: 0x200,
                end: 0x3FF
            }
        );
        assert_eq!(
            parse_range("$1B46E").unwrap(),
            WatchRange {
                start: 0x1_B46E,
                end: 0x1_B46E
            }
        );
    }

    #[test]
    fn rejects_bad_ranges() {
        assert!(parse_range("zz-10").is_err());
        assert!(parse_range("200-100").is_err());
        assert!(parse_range("1000000").is_err());
    }

    #[test]
    fn word_access_overlaps_on_either_byte() {
        let w = WatchRange {
            start: 0x1001,
            end: 0x1001,
        };
        assert!(w.overlaps(0x1000, 2));
        assert!(w.overlaps(0x1001, 1));
        assert!(!w.overlaps(0x1002, 2));
        assert!(!w.overlaps(0x0FFE, 2));
    }

    #[test]
    fn own_fetches_are_the_instruction_bytes_only() {
        // A 6-byte instruction at 0x1000 fetches 0x1000-0x1005.
        assert!(is_own_fetch(0x1000, 0x1000, 6));
        assert!(is_own_fetch(0x1004, 0x1000, 6));
        // A table right after the instruction is data, not a fetch.
        assert!(!is_own_fetch(0x1006, 0x1000, 6));
        assert!(!is_own_fetch(0x0FFE, 0x1000, 6));
    }

    #[test]
    fn the_opcode_is_the_first_fetch_at_the_instruction_address() {
        let log = InstructionAccessLog {
            pc: 0x400,
            accesses: vec![(0x400, 2, false, 0x3039), (0x402, 2, false, 0x0000)],
            ..Default::default()
        };
        assert_eq!(opcode_of(&log), Some(0x3039));
        assert_eq!(opcode_of(&InstructionAccessLog::default()), None);
        let elsewhere = InstructionAccessLog {
            pc: 0x400,
            accesses: vec![(0x500, 2, false, 0xABCD)],
            ..Default::default()
        };
        assert_eq!(opcode_of(&elsewhere), None);
    }

    #[test]
    fn spans_overlap_a_range_on_any_byte() {
        let w = WatchRange {
            start: 0x1000,
            end: 0x10FF,
        };
        assert!(w.overlaps_span(0x0F00, 0x1000));
        assert!(w.overlaps_span(0x10FF, 0x2000));
        assert!(w.overlaps_span(0x0000, 0xFFFF));
        assert!(!w.overlaps_span(0x1100, 0x2000));
        assert!(!w.overlaps_span(0x0000, 0x0FFF));
    }

    #[test]
    fn formats_dma_transfers() {
        let to_vram = MemoryDma {
            source: 0x0D_6000,
            words: 16,
            target: DmaTarget::Vram,
            destination: 0x4000,
        };
        assert_eq!(
            format_dma(&to_vram),
            "DMA $0D6000-$0D601F -> VRAM $4000, 16 words"
        );
        let wrapping = MemoryDma {
            source: 0xFF_FFFE,
            words: 2,
            target: DmaTarget::Cram,
            destination: 0x0000,
        };
        assert_eq!(
            format_dma(&wrapping),
            "DMA $FFFFFE-$FFFFFF + $FE0000-$FE0001 -> CRAM $0000, 2 words"
        );
    }

    #[test]
    fn formats_byte_and_word_reads() {
        assert_eq!(format_read(0xD6EBA, 1, 0x41), "$0D6EBA.b = $41   'A'");
        assert_eq!(format_read(0xD6EBA, 2, 0x416E), "$0D6EBA.w = $416E 'An'");
        assert_eq!(format_read(0x200, 1, 0x00), "$000200.b = $00   '.'");
    }
}
