//! `audio-info`: print a PSG (SN76489) + YM2612 channel-state summary
//! after running a ROM. A `--interval`/`--total-frames` monitoring mode
//! prints the state periodically so you can watch a tune evolve.

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use retro_core::EmulatorCore;
use sg1_core::GenesisEmulator;

#[derive(Args, Debug)]
pub struct AudioInfoArgs {
    /// Path to the ROM file (`.bin` / `.md` / `.gen`).
    pub rom: PathBuf,
    /// Number of frames to run before reporting (default: 60).
    #[arg(short, long, default_value_t = 60)]
    pub frames: u64,
    /// Print state every N frames (live monitoring).
    #[arg(long)]
    pub interval: Option<u64>,
    /// Total frames to run in monitoring mode.
    #[arg(long)]
    pub total_frames: Option<u64>,
}

pub fn run(args: AudioInfoArgs) -> Result<()> {
    let (_, mut emu) = super::load_rom(&args.rom)?;

    println!("=== SG1 Audio Info ===");
    println!("ROM: {}", args.rom.display());
    super::print_rom_title(&emu);

    emu.set_audio_enabled(true);

    if let Some(interval_frames) = args.interval {
        let total = args.total_frames.unwrap_or(600);
        println!("Monitoring mode: printing state every {interval_frames} frames (total: {total})");
        println!();
        for frame in 0..total {
            emu.run_frame_fast();
            emu.clear_audio_buffer();
            if interval_frames > 0 && (frame + 1) % interval_frames == 0 {
                println!("--- Frame {} ---", frame + 1);
                print_audio_state(&emu);
                println!();
            }
        }
    } else {
        println!("Running {} frames...", args.frames);
        for _ in 0..args.frames {
            emu.run_frame_fast();
            emu.clear_audio_buffer();
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
