//! `apu-dump`: run a ROM and summarise S-DSP / SPC700 audio state,
//! with optional ARAM and DSP-register-file binary dumps.
//!
//! The per-voice table (key-on, pitch->Hz, volume, pan, envelope phase)
//! and the global DSP register summary (master volume, KON/KOFF, the
//! FLG reset/mute bits) make it possible to tell from the CLI whether
//! the audio program is running and keying voices, or stalled before
//! the DSP ever produces sound.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use retro_core::audio::{ChannelState, EnvelopePhase};
use retro_core::EmulatorCore;
use snes1_core::SnesEmulator;

// S-DSP global register indices (the per-voice block uses the low
// nibble `0x_0..0x_9` of each high-nibble group; these are the chip-wide
// controls).
const DSP_MVOLL: usize = 0x0C;
const DSP_MVOLR: usize = 0x1C;
const DSP_EVOLL: usize = 0x2C;
const DSP_EVOLR: usize = 0x3C;
const DSP_KON: usize = 0x4C;
const DSP_KOFF: usize = 0x5C;
const DSP_FLG: usize = 0x6C;
const DSP_ENDX: usize = 0x7C;
const DSP_EON: usize = 0x4D;
const DSP_NON: usize = 0x3D;
const DSP_PMON: usize = 0x2D;
const DSP_DIR: usize = 0x5D;

#[derive(Args, Debug)]
pub struct ApuDumpArgs {
    /// Path to the ROM file (`.sfc` / `.smc`).
    pub rom: PathBuf,
    /// Run this many frames before dumping.
    #[arg(long, default_value_t = 300)]
    pub frames: u64,
    /// Dump raw ARAM (64 KiB) to this file.
    #[arg(long)]
    pub aram: Option<PathBuf>,
    /// Dump the 128-byte DSP register file to this file.
    #[arg(long)]
    pub dsp_regs: Option<PathBuf>,
    /// Write the text summary here instead of stdout.
    #[arg(long, short)]
    pub output: Option<PathBuf>,
}

pub fn run(args: ApuDumpArgs) -> Result<()> {
    let rom = super::read_rom(&args.rom)?;
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom)
        .map_err(|e| anyhow::anyhow!("failed to load ROM: {e}"))?;

    for _ in 0..args.frames {
        emu.run_frame();
    }

    // Per-voice state (owned), captured under the mutable trait borrow.
    let states: Vec<ChannelState> = {
        let ad = emu.audio_debugger();
        let n = ad.get_channel_count();
        (0..n).filter_map(|i| ad.get_channel_state(i)).collect()
    };

    // Global DSP register file (copied so the borrow ends immediately).
    let dsp = *emu.bus().apu().dsp_regs();

    if let Some(path) = &args.aram {
        write_bytes(path, emu.bus().apu().aram())?;
    }
    if let Some(path) = &args.dsp_regs {
        write_bytes(path, &dsp)?;
    }

    let mut out = super::open_output(args.output.as_ref())?;
    writeln!(out, "APU state after {} frame(s):", args.frames)?;
    writeln!(
        out,
        "  {:<6} {:<7} {:<7} {:>10} {:>6} {:>7} {:<8} {:>6}",
        "Voice", "Active", "Pitch", "Freq(Hz)", "Vol", "Pan", "Env", "Out"
    )?;
    for st in &states {
        let phase = st
            .envelope
            .as_ref()
            .map(|e| phase_str(e.current_phase))
            .unwrap_or("-");
        writeln!(
            out,
            "  {:<6} {:<7} ${:04X}   {:>10.1} {:>6.2} {:>+7.2} {:<8} {:>6.2}{}",
            st.channel_id,
            if st.enabled { "on" } else { "off" },
            st.frequency_raw,
            st.frequency,
            st.volume,
            st.pan,
            phase,
            st.current_output,
            if st.muted { "  (muted)" } else { "" }
        )?;
    }

    writeln!(out, "  DSP globals:")?;
    writeln!(
        out,
        "    MVOL  L={:>4} R={:>4}    EVOL L={:>4} R={:>4}",
        dsp[DSP_MVOLL] as i8, dsp[DSP_MVOLR] as i8, dsp[DSP_EVOLL] as i8, dsp[DSP_EVOLR] as i8
    )?;
    writeln!(
        out,
        "    FLG   ${:02X}  ({})",
        dsp[DSP_FLG],
        dsp_flag_summary(dsp[DSP_FLG])
    )?;
    writeln!(
        out,
        "    KON   ${:02X}   KOFF ${:02X}   ENDX ${:02X}",
        dsp[DSP_KON], dsp[DSP_KOFF], dsp[DSP_ENDX]
    )?;
    writeln!(
        out,
        "    EON   ${:02X}   NON  ${:02X}   PMON ${:02X}   DIR ${:02X}",
        dsp[DSP_EON], dsp[DSP_NON], dsp[DSP_PMON], dsp[DSP_DIR]
    )?;
    out.flush()?;
    Ok(())
}

fn phase_str(p: EnvelopePhase) -> &'static str {
    match p {
        EnvelopePhase::Attack => "Attack",
        EnvelopePhase::Decay => "Decay",
        EnvelopePhase::Sustain => "Sustain",
        EnvelopePhase::Release => "Release",
        EnvelopePhase::Off => "Off",
    }
}

/// Decode the S-DSP `FLG` ($6C) register into a human-readable summary.
/// Bit 7 = soft reset, bit 6 = mute-all, bit 5 = echo-write disable,
/// bits 4-0 = noise clock rate.
pub(crate) fn dsp_flag_summary(flg: u8) -> String {
    format!(
        "reset={} mute={} echo_off={} noise_clk=${:02X}",
        (flg >> 7) & 1,
        (flg >> 6) & 1,
        (flg >> 5) & 1,
        flg & 0x1F
    )
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes)
        .with_context(|| format!("failed to write dump: {}", path.display()))?;
    eprintln!("wrote {} bytes to {}", bytes.len(), path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_summary_decodes_bits() {
        assert_eq!(
            dsp_flag_summary(0x00),
            "reset=0 mute=0 echo_off=0 noise_clk=$00"
        );
        // Power-on FLG is $E0: reset + mute + echo-write-disable, noise 0.
        assert_eq!(
            dsp_flag_summary(0xE0),
            "reset=1 mute=1 echo_off=1 noise_clk=$00"
        );
        // Mute only, with a noise clock of 0x0A.
        assert_eq!(
            dsp_flag_summary(0x4A),
            "reset=0 mute=1 echo_off=0 noise_clk=$0A"
        );
    }

    #[test]
    fn phase_strings() {
        assert_eq!(phase_str(EnvelopePhase::Attack), "Attack");
        assert_eq!(phase_str(EnvelopePhase::Off), "Off");
    }
}
