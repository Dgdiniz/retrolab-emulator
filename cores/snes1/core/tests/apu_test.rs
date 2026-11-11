//! End-to-end APU foundation tests.
//!
//! These drive the full [`SnesEmulator`] so the whole chain is
//! exercised: the main CPU touches `$2140-$217F`, `SnesBus` intercepts
//! the range and catches the real SPC700 up against its IPL ROM / ARAM
//! bus, and the mailbox latches propagate in both directions. The IPL
//! boot is proven by running real SPC700 opcodes until they write the
//! `$AA`/`$BB` ready handshake the main CPU then reads back.
//!
//! Pure layers are unit-tested next to their modules: exact timer
//! divisors and read-clears in `apu/timers.rs`, the SPC700 memory map
//! in `apu/bus.rs`, and the drift-free sample math in
//! `retro_core::audio::rate_converter`. The cases here pin the
//! integration those layers can't reach, including that the 32 kHz
//! sample cadence stays exactly drift-free over many frames.
//!
//! Reference: bsnes-plus `smp/` (IPL boot + `$F0-$FF` MMIO) and snes9x
//! `apu/apu.cpp` (always-catch-up anti-drift model).

use retro_core::audio::IntegerRateConverter;
use retro_core::cpu::Addressable;
use retro_core::EmulatorCore;
use snes1_core::apu::Apu;
use snes1_core::{SnesEmulator, TimingProfile};

/// Native SPC700 clock (~1.024 MHz).
const SPC_CLOCK_HZ: u64 = 1_024_000;
/// Native S-DSP output sample rate.
const DSP_SAMPLE_RATE_HZ: u64 = 32_000;

/// CPU-bus address of mailbox port 0 (`$00:2140`).
const PORT0: u32 = 0x0000_2140;

// -------------------------------------------------------------
// Minimal LoROM synthesis (reset code only; idles in BRA -2)
// -------------------------------------------------------------

fn stamp_header(chip: &mut [u8], header_offset: usize, reset_vector: u16) {
    let title = b"APU TEST             ";
    chip[header_offset + 0x10..header_offset + 0x10 + 21].copy_from_slice(title);
    chip[header_offset + 0x25] = 0x20; // LoROM
    chip[header_offset + 0x26] = 0x00; // ROM only
    chip[header_offset + 0x27] = 0x0A; // 1 MiB
    chip[header_offset + 0x28] = 0x00; // no SRAM
    chip[header_offset + 0x29] = 0x01; // USA -> NTSC
    let checksum: u16 = 0x1234;
    chip[header_offset + 0x2C..header_offset + 0x2E].copy_from_slice(&(!checksum).to_le_bytes());
    chip[header_offset + 0x2E..header_offset + 0x30].copy_from_slice(&checksum.to_le_bytes());
    chip[header_offset + 0x4C..header_offset + 0x4E].copy_from_slice(&reset_vector.to_le_bytes());
}

/// Build a 1 MiB LoROM whose reset code idles forever (`BRA -2`). The
/// 5A22 does nothing; every test cares only about the APU the bus
/// drives underneath it.
fn build_idle_rom() -> Vec<u8> {
    const SIZE: usize = 0x10_0000;
    let mut chip = vec![0u8; SIZE];
    chip[0x0000..0x0002].copy_from_slice(&[0x80, 0xFE]); // BRA -2 at $8000
    stamp_header(&mut chip, 0x7FB0, 0x8000);
    chip[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes()); // reset vector
    chip
}

fn idle_emulator() -> SnesEmulator {
    let rom = build_idle_rom();
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom).expect("load_rom should succeed");
    assert_eq!(emu.timing(), TimingProfile::NTSC);
    emu
}

/// Master cycles needed for the APU to advance by `spc_cycles` SPC700
/// cycles, at the given region clock.
fn master_for_spc(spc_cycles: u64, master_clock_hz: u64) -> u64 {
    spc_cycles * master_clock_hz / SPC_CLOCK_HZ
}

// -------------------------------------------------------------
// IPL boot + mailbox handshake
// -------------------------------------------------------------

#[test]
fn ipl_handshake_visible_to_cpu_via_2140() {
    let mut emu = idle_emulator();
    // One frame is ~17 000 SPC cycles, far past the IPL's zero-page
    // clear (~2 400) and its $AA/$BB writes. The FrameEnd hook catches
    // the SPC up, so the CPU read of $2140/$2141 sees the ready bytes.
    emu.run_frame();
    assert_eq!(emu.bus_mut().read(PORT0), 0xAA, "$2140 should read IPL $AA");
    assert_eq!(
        emu.bus_mut().read(PORT0 + 1),
        0xBB,
        "$2141 should read IPL $BB"
    );
}

#[test]
fn cpu_write_propagates_to_spc_input_port() {
    let mut emu = idle_emulator();
    emu.run_frame();
    // CPU writes port 2 ($2142); the SPC700 reads it at $F6.
    emu.bus_mut().write(PORT0 + 2, 0x5C);
    assert_eq!(emu.bus_mut().apu_mut().spc_read(0x00F6), 0x5C);
}

#[test]
fn spc_write_propagates_to_cpu_output_port() {
    let mut emu = idle_emulator();
    emu.run_frame();
    // SPC700 writes port 3 ($F7); the CPU reads it at $2143.
    emu.bus_mut().apu_mut().spc_write(0x00F7, 0x77);
    assert_eq!(emu.bus_mut().read(PORT0 + 3), 0x77);
}

#[test]
fn mailbox_mirrors_across_2144_217f() {
    let mut emu = idle_emulator();
    emu.run_frame();
    // Write side: $2151 is a mirror of port 1; the SPC reads it at $F5.
    emu.bus_mut().write(0x0000_2151, 0x63);
    assert_eq!(emu.bus_mut().apu_mut().spc_read(0x00F5), 0x63);
    // Read side: stage a known APU->CPU byte on port 2 and read it back
    // through several $2144-$217F mirrors (offset & 3 == 2).
    emu.bus_mut().apu_mut().spc_write(0x00F6, 0x71);
    for mirror in [0x0000_2142u32, 0x0000_2146, 0x0000_216A, 0x0000_217E] {
        assert_eq!(emu.bus_mut().read(mirror), 0x71, "mirror ${mirror:06X}");
    }
}

#[test]
fn control_bit4_clears_cpu_input_ports() {
    let mut emu = idle_emulator();
    emu.run_frame();
    emu.bus_mut().write(PORT0, 0x12); // cpu->apu port 0
    emu.bus_mut().write(PORT0 + 1, 0x34); // cpu->apu port 1
    let apu = emu.bus_mut().apu_mut();
    assert_eq!(apu.spc_read(0x00F4), 0x12);
    assert_eq!(apu.spc_read(0x00F5), 0x34);
    // A CONTROL ($F1) write with bit 4 set clears input ports 0 and 1.
    apu.spc_write(0x00F1, 0x10);
    assert_eq!(apu.spc_read(0x00F4), 0x00);
    assert_eq!(apu.spc_read(0x00F5), 0x00);
}

// -------------------------------------------------------------
// Timers (integration: wired into catch-up, rate ordering, clear)
// -------------------------------------------------------------

#[test]
fn timers_clocked_by_catch_up_with_correct_rate_ordering() {
    let mut emu = idle_emulator();
    emu.run_frame(); // boot past the IPL into its mailbox spin loop
    let master_clock = emu.timing().master_clock_hz as u64;
    let start = emu.master_cycles();
    {
        let apu = emu.bus_mut().apu_mut();
        apu.spc_write(0x00FA, 1); // timer0 target 1 (8 kHz)
        apu.spc_write(0x00FC, 1); // timer2 target 1 (64 kHz)
        apu.spc_write(0x00F1, 0x85); // enable timer0 + timer2, keep IPL
    }
    // Advance ~160 SPC cycles: timer2 (/16) ticks ~10 times (still
    // below the 4-bit wrap), timer0 (/128) ~once. The spin loop has no
    // timer side effects, so the counts are dominated by the divisors.
    let span = master_for_spc(160, master_clock);
    emu.bus_mut().apu_mut().catch_up_to(start + span);

    let t0 = emu.bus().apu().timer_output(0);
    let t2 = emu.bus().apu().timer_output(2);
    assert_ne!(t2, 0, "64 kHz timer should have ticked during catch-up");
    assert!(
        t2 > t0,
        "64 kHz timer ({t2}) must out-tick the 8 kHz timer ({t0})"
    );

    // A SPC-side read of $FF (timer2 output) returns and clears it.
    let read = emu.bus_mut().apu_mut().spc_read(0x00FF);
    assert_eq!(read, t2);
    assert_eq!(
        emu.bus().apu().timer_output(2),
        0,
        "$FF read must clear the output"
    );
}

// -------------------------------------------------------------
// Reset + drift-free sample cadence
// -------------------------------------------------------------

#[test]
fn reset_reboots_apu_and_reanchors_clocks() {
    let mut emu = idle_emulator();
    for _ in 0..3 {
        emu.run_frame();
    }
    assert!(emu.bus().apu().samples_emitted() > 0);
    assert!(emu.bus().apu().spc_cycles() > 0);
    // Stash a byte in a CPU input port; reset must clear it.
    emu.bus_mut().write(PORT0, 0x9A);

    emu.reset();

    assert_eq!(emu.bus().apu().samples_emitted(), 0);
    assert_eq!(emu.bus().apu().spc_cycles(), 0);
    assert_eq!(emu.bus().apu().spc700().pc, 0xFFC0, "SPC reboots into IPL");
    assert_eq!(
        emu.bus_mut().apu_mut().spc_read(0x00F4),
        0x00,
        "reset clears CPU input ports"
    );
}

#[test]
fn silent_sample_cadence_is_drift_free_over_many_frames() {
    let mut emu = idle_emulator();
    let master_clock = emu.timing().master_clock_hz as u64;
    // The converter's defining property: cumulative samples are an
    // exact integer function of the master cycle. A fresh converter
    // computing the total in one shot is the ground truth; the
    // emulator advancing one frame at a time must agree exactly.
    let reference = IntegerRateConverter::new(DSP_SAMPLE_RATE_HZ, master_clock);
    for _ in 0..600 {
        emu.run_frame();
        let expected = reference.cycles_to_samples(emu.master_cycles());
        assert_eq!(
            emu.bus().apu().samples_emitted(),
            expected,
            "32 kHz sample count must not drift from the master clock"
        );
    }
    // ~10 s of audio at 60 fps must be on the order of 320 000 samples,
    // a sanity bound that the cadence is actually advancing.
    assert!(emu.bus().apu().samples_emitted() > 300_000);
}

// -------------------------------------------------------------
// S-DSP synthesis (dry voices)
// -------------------------------------------------------------
//
// These set up a real looping BRR sample plus its sample directory in
// ARAM, program voice 0 through the `$F2/$F3` register pair, strobe
// KON, and run the DSP via `catch_up_to`. The BRR block decodes to a
// square wave (eight samples at +1792, eight at -1792) so the synthesised
// output is easy to recognise: an audible, bipolar, bounded waveform.

/// Push a DSP register write through the `$F2` (DSPADDR) / `$F3`
/// (DSPDATA) port pair, exactly as the SPC700 audio driver would.
fn dsp_write(apu: &mut Apu, reg: u8, value: u8) {
    apu.spc_write(0x00F2, reg);
    apu.spc_write(0x00F3, value);
}

/// Seed ARAM with a sample directory (entry 0 -> `$0300`, looping) and a
/// 9-byte BRR block that decodes to a +/-1792 square wave. Placed at
/// `$0200+` so the IPL's zero-page clear never touches it.
fn seed_square_sample(apu: &mut Apu) {
    // Sample directory entry 0: start and loop both at $0300.
    apu.spc_write(0x0200, 0x00);
    apu.spc_write(0x0201, 0x03);
    apu.spc_write(0x0202, 0x00);
    apu.spc_write(0x0203, 0x03);
    // BRR block: range 8, filter 0, loop + end flags set.
    apu.spc_write(0x0300, 0x83);
    for a in 0x0301..=0x0304 {
        apu.spc_write(a, 0x77); // nybble +7 -> +1792 (samples 0-7)
    }
    for a in 0x0305..=0x0308 {
        apu.spc_write(a, 0x99); // nybble -7 -> -1792 (samples 8-15)
    }
}

/// Program voice 0 to play the seeded sample at unity pitch with a
/// direct-GAIN full envelope, set the master/voice volumes, unmute, and
/// strobe KON.
fn setup_and_key_on(emu: &mut SnesEmulator, voll: u8, volr: u8, mvol: u8) {
    let apu = emu.bus_mut().apu_mut();
    seed_square_sample(apu);
    dsp_write(apu, 0x00, voll); // VOLL
    dsp_write(apu, 0x01, volr); // VOLR
    dsp_write(apu, 0x02, 0x00); // PITCHL
    dsp_write(apu, 0x03, 0x10); // PITCHH -> pitch 0x1000 (1.0)
    dsp_write(apu, 0x04, 0x00); // SRCN 0
    dsp_write(apu, 0x05, 0x00); // ADSR0 bit 7 clear -> GAIN mode
    dsp_write(apu, 0x07, 0x7f); // GAIN: direct, near-max level
    dsp_write(apu, 0x5d, 0x02); // DIR page -> directory at $0200
    dsp_write(apu, 0x0c, mvol); // MVOLL
    dsp_write(apu, 0x1c, mvol); // MVOLR
    dsp_write(apu, 0x6c, 0x00); // FLG: clear soft reset + mute
    dsp_write(apu, 0x4c, 0x01); // KON voice 0
}

/// Advance the APU by enough master cycles to emit roughly `samples`
/// more 32 kHz stereo samples, keeping an absolute master cursor (the
/// emulator's own `master_cycles` does not move when we drive the APU
/// directly, so a relative target would stall on the catch-up guard).
fn advance_samples(emu: &mut SnesEmulator, cursor: &mut u64, samples: u64) {
    let master_clock = emu.timing().master_clock_hz as u64;
    *cursor += samples * master_clock / DSP_SAMPLE_RATE_HZ;
    emu.bus_mut().apu_mut().catch_up_to(*cursor);
}

/// Drain every queued 32 kHz `i16` sample (left/right interleaved).
fn drain_all(emu: &mut SnesEmulator) -> Vec<i16> {
    let apu = emu.bus_mut().apu_mut();
    let mut buf = vec![0i16; apu.audio_samples_available()];
    apu.drain_audio(&mut buf);
    buf
}

#[test]
fn voice_plays_brr_sample_as_audible_waveform() {
    let mut emu = idle_emulator();
    emu.run_frame();
    let mut cursor = emu.master_cycles();
    drain_all(&mut emu); // discard boot-era silence

    setup_and_key_on(&mut emu, 0x7f, 0x7f, 0x7f);
    advance_samples(&mut emu, &mut cursor, 1500);
    let buf = drain_all(&mut emu);
    assert!(!buf.is_empty(), "the DSP must have produced samples");

    let left: Vec<i16> = buf.iter().step_by(2).copied().collect();
    let max = *left.iter().max().unwrap();
    let min = *left.iter().min().unwrap();
    assert!(max > 500, "expected an audible positive peak, got {max}");
    assert!(min < -500, "expected an audible negative peak, got {min}");
    assert!(
        left.iter().all(|&s| s.abs() < 4000),
        "15-bit DAC output stays bounded"
    );
}

#[test]
fn key_off_releases_voice_to_silence() {
    let mut emu = idle_emulator();
    emu.run_frame();
    let mut cursor = emu.master_cycles();
    drain_all(&mut emu);

    setup_and_key_on(&mut emu, 0x7f, 0x7f, 0x7f);
    advance_samples(&mut emu, &mut cursor, 500);
    drain_all(&mut emu); // discard the playing portion

    // Key off: the release envelope ramps the level to zero (-8/sample
    // from 0x7f0 ~ 254 samples), after which the output is pure silence.
    dsp_write(emu.bus_mut().apu_mut(), 0x5c, 0x01); // KOFF voice 0
    advance_samples(&mut emu, &mut cursor, 1500);
    let buf = drain_all(&mut emu);

    let tail = &buf[buf.len().saturating_sub(200)..];
    assert!(
        tail.iter().all(|&s| s == 0),
        "voice must be silent after the release completes"
    );
}

#[test]
fn endx_flags_the_loop_block() {
    let mut emu = idle_emulator();
    emu.run_frame();
    let mut cursor = emu.master_cycles();
    drain_all(&mut emu);

    setup_and_key_on(&mut emu, 0x7f, 0x7f, 0x7f);
    // One block is 16 samples; after a few blocks the end+loop flags
    // latch ENDX bit 0 for voice 0.
    advance_samples(&mut emu, &mut cursor, 200);
    let endx = emu.bus().apu().dsp_regs()[0x7c];
    assert_eq!(endx & 0x01, 0x01, "ENDX bit 0 set once the block loops");
}

#[test]
fn right_volume_zero_isolates_the_left_channel() {
    let mut emu = idle_emulator();
    emu.run_frame();
    let mut cursor = emu.master_cycles();
    drain_all(&mut emu);

    setup_and_key_on(&mut emu, 0x7f, 0x00, 0x7f); // VOLR = 0
    advance_samples(&mut emu, &mut cursor, 1000);
    let buf = drain_all(&mut emu);

    let left_loud = buf.iter().step_by(2).any(|&s| s.abs() > 500);
    let right_silent = buf.iter().skip(1).step_by(2).all(|&s| s == 0);
    assert!(left_loud, "left channel should be audible");
    assert!(right_silent, "VOLR = 0 mutes the right channel");
}

#[test]
fn master_volume_zero_silences_output() {
    let mut emu = idle_emulator();
    emu.run_frame();
    let mut cursor = emu.master_cycles();
    drain_all(&mut emu);

    setup_and_key_on(&mut emu, 0x7f, 0x7f, 0x00); // MVOL = 0
    advance_samples(&mut emu, &mut cursor, 1000);
    let buf = drain_all(&mut emu);
    assert!(
        buf.iter().all(|&s| s == 0),
        "MVOL = 0 scales the whole mix to silence"
    );
}

#[test]
fn flg_mute_bit_silences_the_dac() {
    let mut emu = idle_emulator();
    emu.run_frame();
    let mut cursor = emu.master_cycles();
    drain_all(&mut emu);

    setup_and_key_on(&mut emu, 0x7f, 0x7f, 0x7f);
    // Re-assert FLG with only the mute bit (6) set: the voice keeps
    // running internally, but the DAC output is forced to zero.
    dsp_write(emu.bus_mut().apu_mut(), 0x6c, 0x40);
    advance_samples(&mut emu, &mut cursor, 1000);
    let buf = drain_all(&mut emu);
    assert!(buf.iter().all(|&s| s == 0), "FLG mute zeroes the DAC");
}

#[test]
fn generated_sample_count_is_drift_free() {
    let mut emu = idle_emulator();
    emu.run_frame();
    let mut cursor = emu.master_cycles();
    drain_all(&mut emu);

    setup_and_key_on(&mut emu, 0x7f, 0x7f, 0x7f);
    let before = emu.bus().apu().samples_emitted();
    advance_samples(&mut emu, &mut cursor, 1000);
    let stereo = (emu.bus().apu().samples_emitted() - before) as usize;

    // The ring received exactly two interleaved i16 per stereo sample,
    // with nothing dropped (well under the ring capacity).
    assert_eq!(emu.bus().apu().audio_samples_available(), 2 * stereo);
}

#[test]
fn reset_clears_audio_ring_and_voices() {
    let mut emu = idle_emulator();
    emu.run_frame();
    let mut cursor = emu.master_cycles();

    setup_and_key_on(&mut emu, 0x7f, 0x7f, 0x7f);
    advance_samples(&mut emu, &mut cursor, 500);
    assert!(
        emu.bus().apu().audio_samples_available() > 0,
        "the voice produced audio before reset"
    );

    emu.reset();
    assert_eq!(
        emu.bus().apu().audio_samples_available(),
        0,
        "reset empties the audio ring"
    );
    assert_eq!(
        emu.bus().apu().dsp_regs()[0x6c],
        0xe0,
        "FLG returns to its power-on value"
    );
    assert_eq!(
        emu.bus().apu().dsp_regs()[0x7c],
        0xff,
        "ENDX returns to its power-on value"
    );
}

// -------------------------------------------------------------
// Echo unit (FIR + feedback + ARAM echo buffer)
// -------------------------------------------------------------
//
// These reuse the square-wave voice but additionally enable the echo
// path: EON routes the voice into the echo accumulator, EFB feeds the
// FIR result back into the ARAM echo buffer (base ESA, length EDL), and
// EVOL scales the echo return into the DAC. The echo buffer is placed
// at `$1000`, clear of the directory (`$0200`) and BRR block (`$0300`).

/// Program voice 0 like `setup_and_key_on` (unity-pitch square wave,
/// full GAIN) and additionally enable the echo unit with echo volume
/// `evol`, ~0.5 feedback, and a 2 KB echo buffer at `$1000`.
fn setup_echo_and_key_on(emu: &mut SnesEmulator, evol: u8) {
    let apu = emu.bus_mut().apu_mut();
    seed_square_sample(apu);
    dsp_write(apu, 0x00, 0x7f); // VOLL
    dsp_write(apu, 0x01, 0x7f); // VOLR
    dsp_write(apu, 0x02, 0x00); // PITCHL
    dsp_write(apu, 0x03, 0x10); // PITCHH -> pitch 1.0
    dsp_write(apu, 0x04, 0x00); // SRCN 0
    dsp_write(apu, 0x05, 0x00); // ADSR0 -> GAIN mode
    dsp_write(apu, 0x07, 0x7f); // GAIN: direct, near-max
    dsp_write(apu, 0x5d, 0x02); // DIR page -> directory at $0200
    dsp_write(apu, 0x0c, 0x7f); // MVOLL
    dsp_write(apu, 0x1c, 0x7f); // MVOLR
    dsp_write(apu, 0x0f, 0x40); // Echo FIR tap 0 = unity ((s * 64) >> 6 == s)
    dsp_write(apu, 0x2c, evol); // EVOLL
    dsp_write(apu, 0x3c, evol); // EVOLR
    dsp_write(apu, 0x0d, 0x40); // EFB ~ 0.5 feedback
    dsp_write(apu, 0x4d, 0x01); // EON voice 0
    dsp_write(apu, 0x6d, 0x10); // ESA -> echo buffer base $1000
    dsp_write(apu, 0x7d, 0x01); // EDL -> 2 KB echo buffer (512 stereo frames)
    dsp_write(apu, 0x6c, 0x00); // FLG: clear reset + mute + echo-write-disable
    dsp_write(apu, 0x4c, 0x01); // KON voice 0
}

/// Snapshot the 2 KB ARAM echo region (`$1000-$17FF`).
fn read_echo_region(apu: &mut Apu) -> Vec<u8> {
    (0x1000u16..0x1800).map(|a| apu.spc_read(a)).collect()
}

/// Boot, set up the echo voice with `evol`, play 1500 stereo samples,
/// and return the drained 32 kHz `i16` output.
fn run_echo_capture(evol: u8) -> Vec<i16> {
    let mut emu = idle_emulator();
    emu.run_frame();
    let mut cursor = emu.master_cycles();
    drain_all(&mut emu);
    setup_echo_and_key_on(&mut emu, evol);
    advance_samples(&mut emu, &mut cursor, 1500);
    drain_all(&mut emu)
}

#[test]
fn echo_feedback_is_written_into_aram() {
    let mut emu = idle_emulator();
    emu.run_frame();
    let mut cursor = emu.master_cycles();
    drain_all(&mut emu);

    setup_echo_and_key_on(&mut emu, 0x7f);
    let before = read_echo_region(emu.bus_mut().apu_mut());
    advance_samples(&mut emu, &mut cursor, 1500);
    let after = read_echo_region(emu.bus_mut().apu_mut());

    assert_ne!(
        before, after,
        "the DSP must write the voice's echo send (via EFB) into the ARAM echo buffer"
    );
}

#[test]
fn echo_return_changes_the_output() {
    // Identical setups that differ only in EVOL must diverge once the
    // echo buffer wraps (after 512 frames) and the echo return starts
    // feeding the DAC. Everything written to ARAM is the same in both
    // runs, EVOL only scales the return into the output mix.
    let with_echo = run_echo_capture(0x7f);
    let no_echo = run_echo_capture(0x00);
    assert_eq!(
        with_echo.len(),
        no_echo.len(),
        "both runs emit the same number of samples"
    );
    assert!(
        with_echo != no_echo,
        "the EVOL echo return must alter the DAC output"
    );
}

#[test]
fn flg_echo_write_disable_leaves_aram_untouched() {
    let mut emu = idle_emulator();
    emu.run_frame();
    let mut cursor = emu.master_cycles();
    drain_all(&mut emu);

    setup_echo_and_key_on(&mut emu, 0x7f);
    // Re-assert FLG with only the echo-write-disable bit (5) set: the
    // FIR still runs, but nothing is written back to the echo buffer.
    dsp_write(emu.bus_mut().apu_mut(), 0x6c, 0x20);
    let before = read_echo_region(emu.bus_mut().apu_mut());
    advance_samples(&mut emu, &mut cursor, 1500);
    let after = read_echo_region(emu.bus_mut().apu_mut());

    assert_eq!(
        before, after,
        "FLG bit 5 must suppress every echo write into ARAM"
    );
}

// -------------------------------------------------------------
// Host-rate audio bridge (32 kHz i16 -> 44.1 kHz f32)
// -------------------------------------------------------------
//
// `run_frame` catches the APU up to the frame boundary and then drains
// the 32 kHz `i16` ring through the linear resampler into the host
// 44.1 kHz `f32` ring, which `get_audio_samples` returns. These pin the
// rate, range, and (crucially) that the resampler adds no drift on top
// of the APU's already drift-free 32 kHz cadence.

/// Host output sample rate (matches `AudioInfo.sample_rate`).
const HOST_SAMPLE_RATE_HZ: usize = 44_100;

#[test]
fn host_audio_bridge_produces_host_rate_f32() {
    let mut emu = idle_emulator();
    emu.set_audio_enabled(true);
    emu.run_frame();
    setup_and_key_on(&mut emu, 0x7f, 0x7f, 0x7f);
    emu.get_audio_samples(); // discard boot + setup-frame audio

    let frames = 60; // ~ 1 second at ~60 fps
    let mut total = 0usize;
    let mut captured: Vec<f32> = Vec::new();
    for _ in 0..frames {
        emu.run_frame();
        let s = emu.get_audio_samples();
        total += s.len();
        captured.extend_from_slice(s);
    }

    // ~44.1 kHz stereo (interleaved) over ~1 s of video frames.
    let nominal = HOST_SAMPLE_RATE_HZ * 2;
    let lo = nominal * 95 / 100;
    let hi = nominal * 105 / 100;
    assert!(
        (lo..=hi).contains(&total),
        "expected ~{nominal} interleaved host samples over 1 s, got {total}"
    );
    assert!(
        captured.iter().all(|&s| (-1.0..=1.0).contains(&s)),
        "host samples must stay in [-1.0, 1.0]"
    );
    assert!(
        captured.iter().any(|&s| s != 0.0),
        "a keyed-on voice must produce non-silent host audio"
    );
}

#[test]
fn host_audio_bridge_is_drift_free() {
    let mut emu = idle_emulator();
    emu.set_audio_enabled(true);
    emu.run_frame();
    setup_and_key_on(&mut emu, 0x7f, 0x7f, 0x7f);
    emu.get_audio_samples(); // discard, then measure a clean window

    let dsp_start = emu.bus().apu().samples_emitted();
    let mut total = 0usize;
    for _ in 0..(60 * 5) {
        emu.run_frame();
        total += emu.get_audio_samples().len();
    }
    let dsp_frames = emu.bus().apu().samples_emitted() - dsp_start;

    // The host count must track the (drift-free) 32 kHz frame count
    // scaled by 44100/32000, interleaved. The only slack is the
    // resampler's carried sub-frame phase, bounded by a couple of
    // frames regardless of how long we run.
    let expected = dsp_frames as f64 * HOST_SAMPLE_RATE_HZ as f64 / 32_000.0 * 2.0;
    let drift = (total as f64 - expected).abs();
    assert!(
        drift <= 8.0,
        "resampler drift {drift} (host={total}, dsp_frames={dsp_frames}, expected~{expected})"
    );
    assert_eq!(
        emu.audio_overflows(),
        0,
        "draining every frame must keep the host ring from overflowing"
    );
}

/// 60-second soak. `#[ignore]` because it runs ~3600 frames; promote it
/// manually (`cargo test -p snes1-core -- --ignored`) or in a nightly job.
/// There is no automated "does it sound correct" assertion: that is
/// checked by ear.
#[test]
#[ignore = "60s audio soak: run manually with --ignored"]
fn apu_no_drift_60s() {
    let mut emu = idle_emulator();
    emu.set_audio_enabled(true);
    emu.run_frame();
    setup_and_key_on(&mut emu, 0x7f, 0x7f, 0x7f);
    emu.get_audio_samples();

    let dsp_start = emu.bus().apu().samples_emitted();
    let mut total = 0usize;
    for _ in 0..(60 * 60) {
        emu.run_frame();
        total += emu.get_audio_samples().len();
    }
    let dsp_frames = emu.bus().apu().samples_emitted() - dsp_start;
    let expected = dsp_frames as f64 * HOST_SAMPLE_RATE_HZ as f64 / 32_000.0 * 2.0;
    assert!(
        (total as f64 - expected).abs() <= 16.0,
        "60 s host drift too large (host={total}, expected~{expected})"
    );
    assert_eq!(emu.audio_overflows(), 0, "no overflow over a 60 s soak");
}
