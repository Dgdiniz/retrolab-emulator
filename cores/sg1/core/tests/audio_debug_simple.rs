//! Simple tests that each sound chip generates one frame of samples from one frame of clocks

use retro_core::audio::AudioChip;
use retro_core::cpu::Clockable;
use sg1_core::psg::Psg;
use sg1_core::ym2612::Ym2612;

#[test]
fn test_psg_clock_and_generate() {
    let mut psg = Psg::new(3_579_545, 44100);

    // Clock for 1 frame worth of cycles
    // At 3.58 MHz, ~3_579_545 / 60 = ~59,659 cycles per frame
    let cycles_per_frame = 59_659;
    psg.clock(cycles_per_frame);

    eprintln!("PSG: Clocked {} cycles", cycles_per_frame);

    // Try to generate samples
    let mut output = vec![0.0; 1000];
    let generated = psg.generate_samples(&mut output, 44100);

    eprintln!("PSG: Generated {} samples", generated);

    assert!(
        generated > 0,
        "PSG must generate samples after being clocked! Got 0 samples."
    );

    // Expected: ~735 samples per frame at 44.1kHz @ 60 FPS
    assert!(
        (700..=800).contains(&generated),
        "PSG should generate ~735 samples per frame, got {}",
        generated
    );
}

#[test]
fn test_ym2612_clock_and_generate() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Clock for 1 frame worth of cycles
    // At 7.67 MHz, ~7_670_454 / 60 = ~127,841 cycles per frame
    let cycles_per_frame = 127_841;
    ym.clock(cycles_per_frame);

    eprintln!("YM2612: Clocked {} cycles", cycles_per_frame);

    // Try to generate samples (stereo interleaved)
    let mut output = vec![0.0; 1500]; // 750 stereo samples = 1500 floats
    let generated = ym.generate_samples(&mut output, 44100);

    eprintln!(
        "YM2612: Generated {} floats ({} stereo samples)",
        generated,
        generated / 2
    );

    assert!(
        generated > 0,
        "YM2612 must generate samples after being clocked! Got 0 samples."
    );

    // Expected: ~735 stereo samples = ~1470 floats per frame
    assert!(
        (1400..=1600).contains(&generated),
        "YM2612 should generate ~1470 floats per frame, got {}",
        generated
    );
}
