//! `sound-trace`: dump every YM2612 + PSG register write with the
//! output-sample offset at which it takes effect, for cross-core
//! sound-timing diffing.
//!
//! SG1 renders audio with a per-write timestamp catch-up model (the chip is
//! run up to a write's cycle *before* the write is applied), as Genesis
//! Plus GX and PicoDrive do. This command exposes that placement: for
//! each frame it prints one line per FM data write and per PSG byte, tagged
//! with the in-frame sample offset the chip recorded when the write arrived.
//! Diffing this stream against the equivalent log from a reference core shows
//! whether SG1 applies each sound write at the same sub-frame position.
//!
//! Line format: `F<frame>:<sample> <decoded write>` (register/data bytes hex).

use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use retro_cli::rom::{load, RomArgs};
use retro_cli::InputArgs;
use retro_core::frame::run_frame_on;
use retro_core::{DebuggerCore, EmulatorCore, ExecutionMode, ExecutionTier};
use sg1_core::psg::PsgWrite;
use sg1_core::ym2612::FmWrite;
use sg1_core::GenesisEmulator;

use crate::console::{Genesis, GenesisBoot};

#[derive(Args, Debug)]
pub struct SoundTraceArgs<B: clap::Args> {
    #[command(flatten)]
    pub rom: RomArgs<B>,
    /// Number of frames to run and trace, e.g. `1`.
    #[arg(long, short)]
    pub frames: Option<u64>,
    /// Write the trace here instead of stdout.
    #[arg(long, short)]
    pub output: Option<PathBuf>,
    #[command(flatten)]
    pub input: InputArgs,
    /// Only trace YM2612 (FM) writes.
    #[arg(long, conflicts_with = "psg_only")]
    pub fm_only: bool,
    /// Only trace PSG (SN76489) writes.
    #[arg(long)]
    pub psg_only: bool,
}

pub fn run(args: SoundTraceArgs<GenesisBoot>) -> Result<()> {
    let Some(frames) = args.frames else {
        anyhow::bail!("specify --frames");
    };
    let input = args.input.plan::<GenesisEmulator>()?;

    let (_, mut emu) = load::<Genesis>(&args.rom)?;
    // Make the CPU actually run (the default mode does not advance under
    // `run_frame`), so the game executes its sound driver, same reason
    // `screenshot`/`fingerprint` set this.
    emu.debugger().set_execution_mode(ExecutionMode::Running);
    // The sound on, as a new machine has it. The sample offsets do not
    // depend on it: they count from the start of each chip's record, which
    // every frame's end takes, the sound on or off.
    emu.set_audio_enabled(true);

    let want_fm = !args.psg_only;
    let want_psg = !args.fm_only;
    if want_fm {
        emu.ym2612_mut().enable_write_log();
    }
    if want_psg {
        emu.psg_mut().enable_write_log();
    }

    let mut out = super::open_output(args.output.as_ref())?;
    writeln!(
        out,
        "; sound register-write trace - F<frame>:<in-frame sample offset> <write>"
    )?;

    let mut total: u64 = 0;
    for frame in 0..frames {
        input.before_frame(&mut emu, frame);
        run_frame_on(&mut emu, ExecutionTier::Play);
        // Drain the rendered audio so the chips' ring buffers don't back up;
        // each write was already timestamped at render time, so this does not
        // affect the captured offsets.
        emu.host_audio_mut().clear();

        let fm = if want_fm {
            emu.ym2612_mut().take_write_log()
        } else {
            Vec::new()
        };
        let psg = if want_psg {
            emu.psg_mut().take_write_log()
        } else {
            Vec::new()
        };

        for line in merge_frame_lines(frame, &fm, &psg) {
            writeln!(out, "{line}")?;
            total += 1;
        }
    }
    out.flush()?;
    eprintln!("Traced {total} sound write(s) over {frames} frame(s).");
    Ok(())
}

/// Format one FM data write: `FM<part> <reg>=<val>` (hex), e.g. `FM0 28=F0`.
fn format_fm(w: &FmWrite) -> String {
    format!("FM{} {:02X}={:02X}", w.part, w.register, w.value)
}

/// Format one PSG byte, decoding the SN76489 command encoding for readability
/// while preserving the raw byte: `PSG <decoded> (raw=<byte>)`.
fn format_psg(w: &PsgWrite) -> String {
    let d = w.data;
    let decoded = if d & 0x80 != 0 {
        // Latch/data byte: channel + type + low nibble.
        let ch = (d >> 5) & 0x03;
        let lo = d & 0x0F;
        match (ch, d & 0x10 != 0) {
            (3, true) => format!("noise vol={lo:X}"),
            (3, false) => format!("noise ctl={lo:X}"),
            (_, true) => format!("ch{ch} vol={lo:X}"),
            (_, false) => format!("ch{ch} freq.lo={lo:X}"),
        }
    } else {
        // Data byte: high 6 bits of a tone channel's frequency.
        format!("freq.hi={:02X}", d & 0x3F)
    };
    format!("PSG {decoded} (raw={d:02X})")
}

/// Merge a frame's FM + PSG writes into time-ordered output lines. Sorting is
/// stable and keyed on `(sample_pos, chip)` so writes at the same offset are
/// deterministic (FM before PSG) and same-chip writes keep chronological
/// order; each line is prefixed `F<frame>:<sample_pos>`.
fn merge_frame_lines(frame: u64, fm: &[FmWrite], psg: &[PsgWrite]) -> Vec<String> {
    let mut items: Vec<(u32, u8, String)> = Vec::with_capacity(fm.len() + psg.len());
    for w in fm {
        items.push((w.sample_pos, 0, format_fm(w)));
    }
    for w in psg {
        items.push((w.sample_pos, 1, format_psg(w)));
    }
    items.sort_by_key(|(pos, chip, _)| (*pos, *chip));
    items
        .into_iter()
        .map(|(pos, _, text)| format!("F{frame}:{pos} {text}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_fm_write_hex() {
        let w = FmWrite {
            sample_pos: 120,
            part: 0,
            register: 0x28,
            value: 0xF0,
            clock: 0,
        };
        assert_eq!(format_fm(&w), "FM0 28=F0");
        let w2 = FmWrite {
            sample_pos: 0,
            part: 1,
            register: 0xB6,
            value: 0x07,
            clock: 0,
        };
        assert_eq!(format_fm(&w2), "FM1 B6=07");
    }

    #[test]
    fn decodes_psg_command_bytes() {
        // 0x9F: latch ch0 volume = 0xF.
        assert_eq!(
            format_psg(&PsgWrite {
                sample_pos: 0,
                data: 0x9F,
                clock: 0,
            }),
            "PSG ch0 vol=F (raw=9F)"
        );
        // 0x80: latch ch0 tone, freq low nibble 0.
        assert_eq!(
            format_psg(&PsgWrite {
                sample_pos: 0,
                data: 0x80,
                clock: 0,
            }),
            "PSG ch0 freq.lo=0 (raw=80)"
        );
        // 0xE5: latch noise control = 5.
        assert_eq!(
            format_psg(&PsgWrite {
                sample_pos: 0,
                data: 0xE5,
                clock: 0,
            }),
            "PSG noise ctl=5 (raw=E5)"
        );
        // 0xFF: latch noise volume = 0xF.
        assert_eq!(
            format_psg(&PsgWrite {
                sample_pos: 0,
                data: 0xFF,
                clock: 0,
            }),
            "PSG noise vol=F (raw=FF)"
        );
        // 0x3F: data byte -> tone frequency high bits.
        assert_eq!(
            format_psg(&PsgWrite {
                sample_pos: 0,
                data: 0x3F,
                clock: 0,
            }),
            "PSG freq.hi=3F (raw=3F)"
        );
    }

    #[test]
    fn merges_in_sample_order_with_frame_prefix() {
        let fm = vec![FmWrite {
            sample_pos: 100,
            part: 0,
            register: 0x28,
            value: 0xF0,
            clock: 0,
        }];
        let psg = vec![PsgWrite {
            sample_pos: 50,
            data: 0x90,
            clock: 0,
        }];
        let lines = merge_frame_lines(7, &fm, &psg);
        assert_eq!(
            lines,
            vec![
                "F7:50 PSG ch0 vol=0 (raw=90)".to_string(),
                "F7:100 FM0 28=F0".to_string(),
            ]
        );
    }

    #[test]
    fn fm_sorts_before_psg_at_equal_offset() {
        let fm = vec![FmWrite {
            sample_pos: 10,
            part: 0,
            register: 0x22,
            value: 0x08,
            clock: 0,
        }];
        let psg = vec![PsgWrite {
            sample_pos: 10,
            data: 0x80,
            clock: 0,
        }];
        let lines = merge_frame_lines(0, &fm, &psg);
        assert_eq!(lines[0], "F0:10 FM0 22=08");
        assert_eq!(lines[1], "F0:10 PSG ch0 freq.lo=0 (raw=80)");
    }

    #[test]
    fn same_chip_keeps_chronological_order_at_equal_offset() {
        // Two FM writes captured at the same sample offset must stay in the
        // order they arrived (stable sort), not get reordered by register.
        let fm = vec![
            FmWrite {
                sample_pos: 5,
                part: 0,
                register: 0x28,
                value: 0xF0,
                clock: 0,
            },
            FmWrite {
                sample_pos: 5,
                part: 0,
                register: 0x28,
                value: 0x00,
                clock: 0,
            },
        ];
        let lines = merge_frame_lines(0, &fm, &[]);
        assert_eq!(lines, vec!["F0:5 FM0 28=F0", "F0:5 FM0 28=00"]);
    }
}
