//! `audio-info`: print a PSG (SN76489) + YM2612 channel-state summary
//! after running a ROM. A `--interval`/`--total-frames` monitoring mode
//! prints the state periodically so you can watch a tune evolve.

use anyhow::Result;
use clap::Args;
use retro_cli::rom::{load, RomArgs};
use retro_core::frame::run_frame_on;
use retro_core::{EmulatorCore, ExecutionTier};
use sg1_core::GenesisEmulator;

use crate::console::{Genesis, GenesisBoot};

#[derive(Args, Debug)]
pub struct AudioInfoArgs<B: clap::Args> {
    #[command(flatten)]
    pub rom: RomArgs<B>,
    /// Number of frames to run before reporting, e.g. `60`; with
    /// `--interval`, `--total-frames` is the run's length instead.
    #[arg(short, long)]
    pub frames: Option<u64>,
    /// Print state every N frames (live monitoring).
    #[arg(long)]
    pub interval: Option<u64>,
    /// Total frames to run in monitoring mode, e.g. `600`.
    #[arg(long)]
    pub total_frames: Option<u64>,
}

pub fn run(args: AudioInfoArgs<GenesisBoot>) -> Result<()> {
    // The run's length is the monitoring mode's total, or the frames.
    let length = match args.interval {
        Some(_) => args
            .total_frames
            .ok_or_else(|| anyhow::anyhow!("specify --total-frames"))?,
        None => args
            .frames
            .ok_or_else(|| anyhow::anyhow!("specify --frames"))?,
    };
    let (_, mut emu) = load::<Genesis>(&args.rom)?;

    println!("=== SG1 Audio Info ===");
    println!("ROM: {}", args.rom.path.display());
    super::print_rom_title(&emu);

    emu.set_audio_enabled(true);

    if let Some(interval_frames) = args.interval {
        let total = length;
        println!("Monitoring mode: printing state every {interval_frames} frames (total: {total})");
        println!();
        for frame in 0..total {
            run_frame_on(&mut emu, ExecutionTier::Play);
            emu.host_audio_mut().clear();
            if interval_frames > 0 && (frame + 1) % interval_frames == 0 {
                println!("--- Frame {} ---", frame + 1);
                print_audio_state(&emu);
                println!();
            }
        }
    } else {
        println!("Running {length} frames...");
        for _ in 0..length {
            run_frame_on(&mut emu, ExecutionTier::Play);
            emu.host_audio_mut().clear();
        }
        println!();
        print_audio_state(&emu);
    }
    Ok(())
}

/// Print the live PSG + YM2612 channel state (frequency, volume, key
/// on/off, FM envelope phase).
fn print_audio_state(emu: &GenesisEmulator) {
    use retro_core::audio::AudioDebugger;

    println!("=== PSG (SN76489) ===");
    let psg = emu.psg();
    for ch in 0..psg.get_channel_count() {
        if let Some(state) = psg.get_channel_state(ch) {
            let active = if state.enabled && state.attenuation < 15 {
                "ACTIVE"
            } else {
                "silent"
            };
            println!(
                "  CH{}: {:?} | freq_raw={:04X} freq={:.1}Hz | atten={} vol={:.2} | {}",
                ch,
                state.channel_type,
                state.frequency_raw,
                state.frequency,
                state.attenuation,
                state.volume,
                active,
            );
        }
    }

    println!("\n=== YM2612 (FM Synthesis) ===");
    let ym = emu.ym2612();
    println!("  LFO: enabled={}", ym.lfo().is_enabled());
    for ch in 0..ym.get_channel_count() {
        if let Some(state) = ym.get_channel_state(ch) {
            let active = if state.enabled { "KEY ON" } else { "key off" };
            println!(
                "  CH{}: {:?} | freq_raw={:04X} freq={:.1}Hz | vol={:.2} pan={:.1} | {}",
                ch,
                state.channel_type,
                state.frequency_raw,
                state.frequency,
                state.volume,
                state.pan,
                active,
            );
            if let Some(ref env) = state.envelope {
                println!(
                    "        envelope: phase={:?} A={:.2} D={:.2} S={:.2} R={:.2}",
                    env.current_phase, env.attack, env.decay, env.sustain, env.release,
                );
            }
        }
    }
}
