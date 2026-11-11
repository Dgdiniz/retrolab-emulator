//! End-to-end save-state + SRAM tests against the full [`SnesEmulator`].
//!
//! These pin the integration the per-component unit tests can't reach:
//! a real ROM is loaded, run for a few frames, snapshotted through the
//! shared [`retro_core::savestate`] envelope (bincode + zstd), and
//! restored into a fresh emulator that must resume bit-for-bit.
//!
//! The synthetic LoROM harness mirrors `controller_test.rs` /
//! `irq_test.rs`: a 1 MiB LoROM whose reset routine programs a little
//! PPU state (VRAM word, a CGRAM backdrop colour), enables NMI +
//! auto-joypad read, turns the display on, and idles in a `BRA -2`
//! loop. Running it advances the CPU, the scheduler, the APU (the IPL
//! boots and the rate converters tick every frame), and the renderer,
//! so a snapshot exercises every captured subsystem.

use retro_core::savestate::{deserialize_compressed, has_magic, serialize_compressed};
use retro_core::EmulatorCore;
use snes1_core::{PresetKind, SnesEmulator, SnesSaveState, TimingProfile, SNES_SAVESTATE_VERSION};

// -------------------------------------------------------------
// ROM synthesis (minimal LoROM, mirrors controller_test.rs)
// -------------------------------------------------------------

const RESET_BASE: usize = 0x0000;
const NMI_HANDLER_OFF: usize = 0x0040;
const NMI_HANDLER_ADDR: u16 = 0x8040;
const IRQ_HANDLER_ADDR: u16 = 0x8060;

fn stamp_header(chip: &mut [u8], header_offset: usize, reset_vector: u16, sram_code: u8) {
    let title = b"SAVESTATE TEST       ";
    chip[header_offset + 0x10..header_offset + 0x10 + 21].copy_from_slice(title);
    chip[header_offset + 0x25] = 0x20; // LoROM
    chip[header_offset + 0x26] = if sram_code == 0 { 0x00 } else { 0x02 }; // ROM (+RAM+battery)
    chip[header_offset + 0x27] = 0x0A; // 1 MiB
    chip[header_offset + 0x28] = sram_code; // sram size code (1024 << code)
    chip[header_offset + 0x29] = 0x01; // USA -> NTSC
    chip[header_offset + 0x2A] = 0x01;
    chip[header_offset + 0x2B] = 0x00;
    let checksum: u16 = 0x1234;
    chip[header_offset + 0x2C..header_offset + 0x2E].copy_from_slice(&(!checksum).to_le_bytes());
    chip[header_offset + 0x2E..header_offset + 0x30].copy_from_slice(&checksum.to_le_bytes());
    chip[header_offset + 0x4C..header_offset + 0x4E].copy_from_slice(&reset_vector.to_le_bytes());
}

/// Build a 1 MiB LoROM with the given reset code, an `RTI` NMI handler,
/// the chosen SRAM size code, and an extra `marker` byte stamped past
/// the header so two otherwise-identical ROMs hash differently.
fn build_rom(reset_code: &[u8], sram_code: u8, marker: u8) -> Vec<u8> {
    const SIZE: usize = 0x10_0000;
    let mut chip = vec![0u8; SIZE];
    chip[RESET_BASE..RESET_BASE + reset_code.len()].copy_from_slice(reset_code);
    // NMI handler: bare RTI (one NMI per frame; nothing to do).
    chip[NMI_HANDLER_OFF] = 0x40;
    // IRQ handler: lone RTI at $8060, never armed here.
    chip[0x0060] = 0x40;
    // Distinguishing marker in an unused ROM region -> distinct SHA-256.
    chip[0x0100] = marker;
    stamp_header(&mut chip, 0x7FB0, 0x8000, sram_code);
    chip[0x7FFA..0x7FFC].copy_from_slice(&NMI_HANDLER_ADDR.to_le_bytes());
    chip[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes());
    chip[0x7FFE..0x8000].copy_from_slice(&IRQ_HANDLER_ADDR.to_le_bytes());
    chip
}

// -------------------------------------------------------------
// Hand-assembled reset routine (65C816, emulation mode / 8-bit)
// -------------------------------------------------------------

/// Reset routine that leaves the machine in a non-trivial, fully
/// deterministic state:
///
/// ```text
/// LDA #$80 / STA $2100      ; forced blank while we program VRAM/CGRAM
/// LDA #$00 / STA $2116 / STA $2117   ; VMADD = 0
/// LDA #$AB / STA $2118      ; VMDATAL
/// LDA #$CD / STA $2119      ; VMDATAH (writes word $CDAB, auto-increment)
/// LDA #$00 / STA $2121      ; CGADD = 0 (backdrop colour index)
/// LDA #$1F / STA $2122      ; CGDATA low
/// LDA #$7C / STA $2122      ; CGDATA high (backdrop = $7C1F)
/// LDA #$81 / STA $4200      ; enable NMI (b7) + auto-joypad read (b0)
/// LDA #$0F / STA $2100      ; display on, full brightness
/// BRA -2                    ; idle
/// ```
#[rustfmt::skip]
const RESET_INIT: [u8; 50] = [
    0xA9, 0x80, 0x8D, 0x00, 0x21, // LDA #$80 / STA $2100
    0xA9, 0x00, 0x8D, 0x16, 0x21, 0x8D, 0x17, 0x21, // LDA #$00 / STA $2116 / STA $2117
    0xA9, 0xAB, 0x8D, 0x18, 0x21, // LDA #$AB / STA $2118
    0xA9, 0xCD, 0x8D, 0x19, 0x21, // LDA #$CD / STA $2119
    0xA9, 0x00, 0x8D, 0x21, 0x21, // LDA #$00 / STA $2121
    0xA9, 0x1F, 0x8D, 0x22, 0x21, // LDA #$1F / STA $2122
    0xA9, 0x7C, 0x8D, 0x22, 0x21, // LDA #$7C / STA $2122
    0xA9, 0x81, 0x8D, 0x00, 0x42, // LDA #$81 / STA $4200
    0xA9, 0x0F, 0x8D, 0x00, 0x21, // LDA #$0F / STA $2100
    0x80, 0xFE,                   // BRA -2
];

fn loaded(sram_code: u8, marker: u8) -> SnesEmulator {
    let rom = build_rom(&RESET_INIT, sram_code, marker);
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom).expect("load_rom should succeed");
    assert_eq!(emu.timing(), TimingProfile::NTSC);
    emu
}

/// Compact view of the observable state used to compare two emulators.
#[derive(Debug, PartialEq, Eq)]
struct Observed {
    pc: u16,
    pbr: u8,
    master_cycles: u64,
    frame_count: u64,
    spc_cycles: u64,
    samples_emitted: u64,
    framebuffer: Vec<u8>,
}

fn observe(emu: &SnesEmulator) -> Observed {
    Observed {
        pc: emu.cpu().pc,
        pbr: emu.cpu().pbr,
        master_cycles: emu.master_cycles(),
        frame_count: emu.frame_count(),
        spc_cycles: emu.bus().apu().spc_cycles(),
        samples_emitted: emu.bus().apu().samples_emitted(),
        framebuffer: emu.get_framebuffer().data.clone(),
    }
}

// -------------------------------------------------------------
// Deterministic resume
// -------------------------------------------------------------

#[test]
fn save_load_resumes_deterministically() {
    let mut emu_a = loaded(0, 0x11);
    emu_a.set_audio_enabled(true);
    for _ in 0..3 {
        emu_a.run_frame();
    }
    let snapshot = emu_a.save_state();
    assert!(!snapshot.is_empty());

    // Fresh emulator with the same ROM, restored from the snapshot.
    let mut emu_b = loaded(0, 0x11);
    emu_b.set_audio_enabled(true);
    emu_b
        .load_state(&snapshot)
        .expect("load_state should succeed");

    // Run both forward identically; a correct restore stays in lockstep.
    for _ in 0..2 {
        emu_a.run_frame();
        emu_b.run_frame();
    }

    assert_eq!(
        observe(&emu_a),
        observe(&emu_b),
        "restored emulator must resume bit-for-bit (PC, cycles, APU, framebuffer)"
    );
}

#[test]
fn restore_is_self_consistent_resave_matches() {
    // A snapshot taken immediately after restoring the same snapshot
    // must serialize to the identical bytes, this catches any field
    // that is captured but not (or wrongly) restored, because the
    // re-save reflects the restored emulator's actual live state.
    let mut emu_a = loaded(0, 0x22);
    for _ in 0..4 {
        emu_a.run_frame();
    }
    let snapshot = emu_a.save_state();

    let mut emu_b = loaded(0, 0x22);
    emu_b
        .load_state(&snapshot)
        .expect("load_state should succeed");
    let resave = emu_b.save_state();

    assert_eq!(
        snapshot, resave,
        "re-saving immediately after a restore must reproduce the same bytes"
    );
}

// -------------------------------------------------------------
// Envelope shape + size
// -------------------------------------------------------------

#[test]
fn save_state_carries_magic_and_is_reasonably_small() {
    let mut emu = loaded(0, 0x33);
    for _ in 0..2 {
        emu.run_frame();
    }
    let bytes = emu.save_state();
    assert!(has_magic(&bytes), "savestate must carry the RLSS magic");
    // The payload is ~256 KiB of mostly-zero RAM/VRAM/ARAM before zstd,
    // so a synthetic state compresses to a few KiB. A state of a real
    // game (Super Mario World) stays well under 2 MiB; the synthetic
    // one is far smaller, so 1 MiB is a generous ceiling.
    assert!(
        bytes.len() < 1024 * 1024,
        "synthetic savestate unexpectedly large: {} bytes",
        bytes.len()
    );
}

// -------------------------------------------------------------
// Integrity gates
// -------------------------------------------------------------

#[test]
fn rom_hash_mismatch_rejects_load() {
    // emu_a / emu_b load ROMs that differ only by their marker byte, so
    // their SHA-256s differ and the envelope's rom_hash gate trips.
    let mut emu_a = loaded(0, 0xA1);
    emu_a.run_frame();
    let snapshot = emu_a.save_state();

    let mut emu_b = loaded(0, 0xB2);
    let err = emu_b
        .load_state(&snapshot)
        .expect_err("loading a state from a different ROM must fail");
    assert!(err.contains("ROM hash mismatch"), "unexpected error: {err}");
}

#[test]
fn any_rom_loads_a_state_made_on_another_rom_and_resumes_it() {
    use retro_core::savestate::{LoadStateError, RomCheck};

    // Two builds of one program, told apart by their marker byte: a state
    // saved on the first resumes on the second when the host asks for any
    // ROM, exactly where it was saved.
    let mut emu_a = loaded(0, 0xA1);
    for _ in 0..3 {
        emu_a.run_frame();
    }
    let snapshot = emu_a.save_state();

    let mut emu_b = loaded(0, 0xB2);
    let err = emu_b
        .load_state_with_rom_check(&snapshot, RomCheck::SameRom)
        .expect_err("the same-ROM check refuses another ROM's state");
    assert!(
        matches!(err, LoadStateError::OtherRom { .. }),
        "got {err:?}"
    );
    emu_b
        .load_state_with_rom_check(&snapshot, RomCheck::AnyRom)
        .expect("any ROM takes the state");
    assert_eq!(emu_b.frame_count(), emu_a.frame_count());
    assert_eq!(emu_b.state_hash(), emu_a.state_hash());

    // Bytes that are no savestate stay refused, whatever the ROM check.
    let garbage = emu_b
        .load_state_with_rom_check(&[0xAA, 0xBB, 0xCC, 0xDD], RomCheck::AnyRom)
        .expect_err("garbage is not a savestate");
    assert!(
        matches!(garbage, LoadStateError::Unreadable(_)),
        "got {garbage:?}"
    );
}

#[test]
fn version_too_new_rejects_load() {
    let mut emu = loaded(0, 0x44);
    emu.run_frame();
    let bytes = emu.save_state();

    // Tamper the payload schema version to one past what we support and
    // re-seal the envelope; the loader must refuse it.
    let mut env = deserialize_compressed::<SnesSaveState>(&bytes).expect("decode");
    env.payload.version = SNES_SAVESTATE_VERSION + 1;
    let tampered = serialize_compressed(&env, 3).expect("re-encode");
    let err = emu
        .load_state(&tampered)
        .expect_err("a newer payload version must be rejected");
    assert!(err.contains("newer"), "unexpected error: {err}");
}

#[test]
fn envelope_core_version_too_new_rejects_load() {
    let mut emu = loaded(0, 0x45);
    emu.run_frame();
    let bytes = emu.save_state();

    // Leave the payload at its current version but bump the envelope's
    // core_version; the loader guards that field too so a tampered
    // wrapper can't smuggle a future schema past the payload check.
    let mut env = deserialize_compressed::<SnesSaveState>(&bytes).expect("decode");
    env.core_version = SNES_SAVESTATE_VERSION + 5;
    let tampered = serialize_compressed(&env, 3).expect("re-encode");
    assert!(
        emu.load_state(&tampered).is_err(),
        "a newer envelope core_version must be rejected"
    );
}

#[test]
fn load_state_rejects_non_envelope_bytes() {
    let mut emu = loaded(0, 0x46);
    assert!(emu.load_state(&[]).is_err(), "empty input has no magic");
    assert!(
        emu.load_state(&[1, 2, 3, 4, 5, 6]).is_err(),
        "garbage input has no magic"
    );
}

// -------------------------------------------------------------
// SRAM persistence
// -------------------------------------------------------------

#[test]
fn sram_persists_through_get_set_save_ram() {
    // sram_code 3 -> 1024 << 3 = 8 KiB of battery SRAM.
    let mut emu = loaded(3, 0x55);
    let sram = emu
        .get_save_ram()
        .expect("an 8 KiB-SRAM cartridge exposes save RAM");
    assert_eq!(sram.len(), 8192, "SRAM size = 1024 << 3");
    assert!(sram.iter().all(|&b| b == 0), "SRAM powers up zeroed");

    // Write a recognisable pattern and read it back through a *fresh*
    // emulator, mirroring how the host persists `.srm` between sessions.
    let mut pattern = vec![0u8; 8192];
    for (i, b) in pattern.iter_mut().enumerate() {
        *b = (i % 251) as u8;
    }
    emu.set_save_ram(&pattern);
    assert_eq!(emu.get_save_ram().unwrap(), &pattern[..]);

    let mut fresh = loaded(3, 0x55);
    fresh.set_save_ram(&pattern);
    assert_eq!(fresh.get_save_ram().unwrap(), &pattern[..]);
}

#[test]
fn sram_round_trips_through_a_savestate() {
    let mut emu = loaded(3, 0x66);
    let mut pattern = vec![0u8; 8192];
    for (i, b) in pattern.iter_mut().enumerate() {
        *b = (i.wrapping_mul(7) % 256) as u8;
    }
    emu.set_save_ram(&pattern);
    emu.run_frame();
    let snapshot = emu.save_state();

    // A fresh emulator with the same ROM (SRAM zeroed) must recover the
    // written SRAM bytes from the savestate.
    let mut restored = loaded(3, 0x66);
    assert!(restored.get_save_ram().unwrap().iter().all(|&b| b == 0));
    restored.load_state(&snapshot).expect("load_state");
    assert_eq!(
        restored.get_save_ram().unwrap(),
        &pattern[..],
        "savestate must carry battery SRAM"
    );
}

#[test]
fn no_sram_cartridge_reports_none() {
    let emu = loaded(0, 0x77);
    assert!(
        emu.get_save_ram().is_none(),
        "a ROM-only cartridge exposes no save RAM"
    );
}

// -------------------------------------------------------------
// DSP-1 coprocessor savestate (synthetic LoROM DSP-1 + firmware), the
// LLE, so only with the `nec-lle` feature; the HLE's state has its own tests
// -------------------------------------------------------------

/// Reset routine for the DSP-1 harness: forced blank, then drive the
/// DSP-1 register window (write DR, read SR, read DR via absolute-long
/// so bank/DBR don't matter), enable NMI + display, and idle.
///
/// ```text
/// LDA #$80 / STA $2100        ; forced blank
/// LDA #$BE / STA $208000      ; DR write (bank $20, $8000 = DR, bit14 clear)
/// LDA $20C000                 ; SR read  ($C000 = SR, bit14 set)
/// LDA $208000                 ; DR read
/// LDA #$81 / STA $4200        ; NMI + auto-joypad
/// LDA #$0F / STA $2100        ; display on
/// BRA -2                      ; idle
/// ```
#[cfg(feature = "nec-lle")]
#[rustfmt::skip]
const RESET_DSP: [u8; 31] = [
    0xA9, 0x80, 0x8D, 0x00, 0x21,       // LDA #$80 / STA $2100
    0xA9, 0xBE, 0x8F, 0x00, 0x80, 0x20, // LDA #$BE / STA $208000 (DR write)
    0xAF, 0x00, 0xC0, 0x20,             // LDA $20C000 (SR read)
    0xAF, 0x00, 0x80, 0x20,             // LDA $208000 (DR read)
    0xA9, 0x81, 0x8D, 0x00, 0x42,       // LDA #$81 / STA $4200
    0xA9, 0x0F, 0x8D, 0x00, 0x21,       // LDA #$0F / STA $2100
    0x80, 0xFE,                         // BRA -2
];

/// 512 KiB LoROM whose header `detect_dsp1_layout` accepts as a LoROM
/// DSP-1 cart (map-mode $20, cartridge type $03, <= 1 MiB -> `LoRom1Mb`).
/// `marker` distinguishes two builds so their SHA-256s differ.
#[cfg(feature = "nec-lle")]
fn build_dsp1_rom(reset_code: &[u8], marker: u8) -> Vec<u8> {
    const SIZE: usize = 0x8_0000;
    let mut chip = vec![0u8; SIZE];
    chip[RESET_BASE..RESET_BASE + reset_code.len()].copy_from_slice(reset_code);
    chip[NMI_HANDLER_OFF] = 0x40; // NMI handler: bare RTI
    chip[0x0060] = 0x40; // IRQ handler: bare RTI
    chip[0x0100] = marker;

    let off = 0x7FB0;
    chip[off + 0x10..off + 0x10 + 21].copy_from_slice(b"DSP1 SAVESTATE TEST  ");
    chip[off + 0x25] = 0x20; // LoROM
    chip[off + 0x26] = 0x03; // ROM + DSP-1 coprocessor
    chip[off + 0x27] = 0x09; // 512 KiB
    chip[off + 0x28] = 0x00; // no SRAM
    chip[off + 0x29] = 0x01; // USA -> NTSC
    chip[off + 0x2A] = 0x01;
    chip[off + 0x2B] = 0x00;
    let checksum: u16 = 0x1234;
    chip[off + 0x2C..off + 0x2E].copy_from_slice(&(!checksum).to_le_bytes());
    chip[off + 0x2E..off + 0x30].copy_from_slice(&checksum.to_le_bytes());
    chip[0x7FFA..0x7FFC].copy_from_slice(&NMI_HANDLER_ADDR.to_le_bytes());
    chip[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes());
    chip[0x7FFE..0x8000].copy_from_slice(&IRQ_HANDLER_ADDR.to_le_bytes());
    chip
}

/// Synthetic DSP-1 firmware (8192 bytes, little-endian `.rom` layout):
/// program word 0 loads a recognisable constant into `DR` (so the chip
/// state is non-trivial), every other program word self-loops so the PC
/// stays bounded and deterministic. Data ROM is left zero.
#[cfg(feature = "nec-lle")]
fn synth_dsp1_firmware() -> Vec<u8> {
    const PROGRAM_WORDS: usize = 2048;
    let ld = |id: u32, dst: u32| (3u32 << 22) | (id << 6) | dst;
    let ljmp = |addr: u32| (2u32 << 22) | (0x100u32 << 13) | (addr << 2);

    let mut program = vec![0u32; PROGRAM_WORDS];
    for (i, slot) in program.iter_mut().enumerate() {
        *slot = ljmp(i as u32);
    }
    program[0] = ld(0xBEEF, 6); // LD #0xBEEF -> DR

    let mut blob = vec![0u8; 8192];
    for (i, &w) in program.iter().enumerate() {
        let b = &mut blob[i * 3..i * 3 + 3];
        b[2] = (w >> 16) as u8;
        b[1] = (w >> 8) as u8;
        b[0] = w as u8;
    }
    blob
}

#[cfg(feature = "nec-lle")]
fn loaded_dsp1(marker: u8) -> SnesEmulator {
    let rom = build_dsp1_rom(&RESET_DSP, marker);
    let mut emu = SnesEmulator::new();
    emu.set_coprocessor_firmware(Some(synth_dsp1_firmware()));
    emu.load_rom(&rom).expect("DSP-1 load_rom should succeed");
    assert!(
        emu.coprocessor_label().unwrap_or("").contains("LLE"),
        "synthetic firmware should select the LLE backend"
    );
    emu
}

/// The DSP-1's restorable observables: clock count + program counter.
#[cfg(feature = "nec-lle")]
fn dsp_cycle_and_pc(emu: &SnesEmulator) -> (u64, Option<u32>) {
    let st = emu
        .coprocessor_debug_state()
        .expect("DSP-1 cartridge exposes chip state");
    (st.cycle_count, st.program_counter)
}

#[cfg(feature = "nec-lle")]
#[test]
fn dsp1_save_load_resumes_deterministically() {
    let mut emu_a = loaded_dsp1(0x11);
    emu_a.set_audio_enabled(true);
    for _ in 0..3 {
        emu_a.run_frame();
    }
    // The reset routine drove the DR/SR window, so the chip has run.
    assert!(
        emu_a.coprocessor_accesses() > 0,
        "reset routine must touch the DSP-1 register window"
    );
    let (cyc_a, _) = dsp_cycle_and_pc(&emu_a);
    assert!(cyc_a > 0, "DSP-1 must have executed instructions");

    let snapshot = emu_a.save_state();

    let mut emu_b = loaded_dsp1(0x11);
    emu_b.set_audio_enabled(true);
    emu_b
        .load_state(&snapshot)
        .expect("load_state should succeed");

    // The restore must reinstate the chip's cycle count + PC immediately.
    assert_eq!(
        dsp_cycle_and_pc(&emu_b),
        dsp_cycle_and_pc(&emu_a),
        "DSP-1 cycle count + PC must survive the savestate"
    );

    // Both must then advance in lockstep.
    for _ in 0..2 {
        emu_a.run_frame();
        emu_b.run_frame();
    }
    assert_eq!(
        observe(&emu_a),
        observe(&emu_b),
        "restored emulator (with DSP-1) must resume bit-for-bit"
    );
    assert_eq!(
        dsp_cycle_and_pc(&emu_a),
        dsp_cycle_and_pc(&emu_b),
        "DSP-1 must stay in lockstep after restore"
    );
}

#[cfg(feature = "nec-lle")]
#[test]
fn dsp1_restore_is_self_consistent_resave_matches() {
    // The coprocessor blob must be both captured *and* restored: if any
    // uPD7725 field or the converter anchor were dropped, the immediate
    // re-save would diverge from the original bytes.
    let mut emu_a = loaded_dsp1(0x22);
    for _ in 0..4 {
        emu_a.run_frame();
    }
    let snapshot = emu_a.save_state();

    let mut emu_b = loaded_dsp1(0x22);
    emu_b
        .load_state(&snapshot)
        .expect("load_state should succeed");
    let resave = emu_b.save_state();

    assert_eq!(
        snapshot, resave,
        "re-saving immediately after a DSP-1 restore must reproduce the same bytes"
    );
}

// -------------------------------------------------------------
// S-DD1 coprocessor savestate (synthetic LoROM, identifier $4532)
// -------------------------------------------------------------

/// Reset routine for the S-DD1 harness: forced blank, program the MMC
/// latch + both enable masks through the `$4800-$480F` register window,
/// seed one DMA channel's `$43xx` shadow (spied via `observe_write`),
/// enable NMI + display, and idle. There is no `$420B` kick, so nothing
/// decompresses, the chip just holds a non-trivial mapping state worth
/// snapshotting.
///
/// ```text
/// LDA #$80 / STA $2100        ; forced blank
/// LDA #$01 / STA $4804        ; mmc[0] = super-bank 1
/// LDA #$03 / STA $4800        ; sdd1_enable = channels 0,1
/// LDA #$01 / STA $4801        ; xfer_enable = channel 0
/// LDA #$45 / STA $4302        ; ch0 A-addr lo
/// LDA #$23 / STA $4303        ; ch0 A-addr mid
/// LDA #$C1 / STA $4304        ; ch0 A-addr bank  -> src $C12345
/// LDA #$34 / STA $4305        ; ch0 size lo
/// LDA #$12 / STA $4306        ; ch0 size hi      -> size $1234
/// LDA #$81 / STA $4200        ; NMI + auto-joypad
/// LDA #$0F / STA $2100        ; display on
/// BRA -2                      ; idle
/// ```
#[rustfmt::skip]
const RESET_SDD1: [u8; 57] = [
    0xA9, 0x80, 0x8D, 0x00, 0x21, // LDA #$80 / STA $2100
    0xA9, 0x01, 0x8D, 0x04, 0x48, // LDA #$01 / STA $4804
    0xA9, 0x03, 0x8D, 0x00, 0x48, // LDA #$03 / STA $4800
    0xA9, 0x01, 0x8D, 0x01, 0x48, // LDA #$01 / STA $4801
    0xA9, 0x45, 0x8D, 0x02, 0x43, // LDA #$45 / STA $4302
    0xA9, 0x23, 0x8D, 0x03, 0x43, // LDA #$23 / STA $4303
    0xA9, 0xC1, 0x8D, 0x04, 0x43, // LDA #$C1 / STA $4304
    0xA9, 0x34, 0x8D, 0x05, 0x43, // LDA #$34 / STA $4305
    0xA9, 0x12, 0x8D, 0x06, 0x43, // LDA #$12 / STA $4306
    0xA9, 0x81, 0x8D, 0x00, 0x42, // LDA #$81 / STA $4200
    0xA9, 0x0F, 0x8D, 0x00, 0x21, // LDA #$0F / STA $2100
    0x80, 0xFE,                   // BRA -2
];

/// 1 MiB LoROM whose header identifier is `$4532` (cartridge_type $45,
/// map_mode $32) so the detector promotes it to an S-DD1 cartridge with
/// 8 KiB SRAM (matching Star Ocean's board). `marker` distinguishes two
/// builds so their SHA-256s differ.
fn build_sdd1_rom(reset_code: &[u8], marker: u8) -> Vec<u8> {
    const SIZE: usize = 0x10_0000;
    let mut chip = vec![0u8; SIZE];
    chip[RESET_BASE..RESET_BASE + reset_code.len()].copy_from_slice(reset_code);
    chip[NMI_HANDLER_OFF] = 0x40; // NMI handler: bare RTI
    chip[0x0060] = 0x40; // IRQ handler: bare RTI
    chip[0x0100] = marker;

    let off = 0x7FB0;
    chip[off + 0x10..off + 0x10 + 21].copy_from_slice(b"SDD1 SAVESTATE TEST  ");
    chip[off + 0x25] = 0x32; // map_mode (ROMSpeed): FastROM + S-DD1 code
    chip[off + 0x26] = 0x45; // cartridge_type (ROMType) -> identifier $4532
    chip[off + 0x27] = 0x0A; // 1 MiB
    chip[off + 0x28] = 0x03; // 8 KiB SRAM (1024 << 3)
    chip[off + 0x29] = 0x01; // USA -> NTSC
    chip[off + 0x2A] = 0x01;
    chip[off + 0x2B] = 0x00;
    let checksum: u16 = 0x1234;
    chip[off + 0x2C..off + 0x2E].copy_from_slice(&(!checksum).to_le_bytes());
    chip[off + 0x2E..off + 0x30].copy_from_slice(&checksum.to_le_bytes());
    chip[0x7FFA..0x7FFC].copy_from_slice(&NMI_HANDLER_ADDR.to_le_bytes());
    chip[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes());
    chip[0x7FFE..0x8000].copy_from_slice(&IRQ_HANDLER_ADDR.to_le_bytes());
    chip
}

fn loaded_sdd1(marker: u8) -> SnesEmulator {
    let rom = build_sdd1_rom(&RESET_SDD1, marker);
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom).expect("S-DD1 load_rom should succeed");
    assert_eq!(
        emu.preset_kind(),
        Some(PresetKind::Sdd1),
        "identifier $4532 must detect as S-DD1"
    );
    assert_eq!(emu.coprocessor_label(), Some("S-DD1"));
    emu
}

/// The S-DD1's restorable mapping observables (MMC latches, enable masks,
/// DECOMPS, and channel 0's DMA shadow), read out of the debug state. The
/// `ACCESSES` diagnostic is dropped: it is deliberately not persisted, so
/// a restored emulator legitimately differs from the original there.
fn sdd1_mapping(emu: &SnesEmulator) -> Vec<(String, u32)> {
    emu.coprocessor_debug_state()
        .expect("S-DD1 cartridge exposes chip state")
        .registers
        .into_iter()
        .filter(|r| r.name != "ACCESSES")
        .map(|r| (r.name, r.value))
        .collect()
}

#[test]
fn sdd1_save_load_resumes_deterministically() {
    let mut emu_a = loaded_sdd1(0x11);
    emu_a.set_audio_enabled(true);
    for _ in 0..3 {
        emu_a.run_frame();
    }
    // The reset routine drove the register window.
    assert!(
        emu_a.coprocessor_accesses() > 0,
        "reset routine must touch the S-DD1 register window"
    );
    let mapping_a = sdd1_mapping(&emu_a);
    // MMC0 was reprogrammed to super-bank 1 and channel 0's shadow seeded.
    assert!(
        mapping_a.contains(&("MMC0".to_string(), 1)),
        "MMC0 reprogrammed, got {mapping_a:?}"
    );
    assert!(
        mapping_a.contains(&("DMA0".to_string(), 0xC1_2345)),
        "channel 0 DMA shadow seeded, got {mapping_a:?}"
    );

    let snapshot = emu_a.save_state();

    let mut emu_b = loaded_sdd1(0x11);
    emu_b.set_audio_enabled(true);
    emu_b
        .load_state(&snapshot)
        .expect("load_state should succeed");

    // The restore reinstated the whole mapping immediately.
    assert_eq!(
        sdd1_mapping(&emu_b),
        mapping_a,
        "S-DD1 mapping state must survive the savestate"
    );

    // Both must then advance in lockstep.
    for _ in 0..2 {
        emu_a.run_frame();
        emu_b.run_frame();
    }
    assert_eq!(
        observe(&emu_a),
        observe(&emu_b),
        "restored emulator (with S-DD1) must resume bit-for-bit"
    );
    assert_eq!(sdd1_mapping(&emu_a), sdd1_mapping(&emu_b));
}

#[test]
fn sdd1_restore_is_self_consistent_resave_matches() {
    // If any persisted S-DD1 field were dropped on the way through the
    // savestate, the immediate re-save would diverge from the original.
    let mut emu_a = loaded_sdd1(0x22);
    for _ in 0..4 {
        emu_a.run_frame();
    }
    let snapshot = emu_a.save_state();

    let mut emu_b = loaded_sdd1(0x22);
    emu_b
        .load_state(&snapshot)
        .expect("load_state should succeed");
    let resave = emu_b.save_state();

    assert_eq!(
        snapshot, resave,
        "re-saving immediately after an S-DD1 restore must reproduce the same bytes"
    );
}

// -------------------------------------------------------------
// SA-1 coprocessor savestate (synthetic LoROM SA-1, map_mode $23)
// -------------------------------------------------------------

/// The SA-1's own program, executed by the second 65816 through its ROM
/// window. A tight `INC $10 / BRA` loop: it keeps the SA-1 busy and
/// mutates I-RAM `$10` every iteration, so the second CPU's PC, cycle
/// count, and I-RAM all evolve, and must survive a savestate.
///
/// ```text
/// INC $10        ; E6 10   bump I-RAM direct-page $10
/// BRA -4         ; 80 FC   loop back to the INC
/// ```
#[cfg(feature = "chip-sa1")]
#[rustfmt::skip]
const SA1_PROGRAM: [u8; 4] = [0xE6, 0x10, 0x80, 0xFC];

/// SA-1 program entry as seen on the SA-1 bus. The block-C fixed
/// projection maps `$00:8000+X -> rom[X]`, and `SA1_PROGRAM` is stamped at
/// rom[$0400], so the SA-1 reset vector (CRV) points here.
#[cfg(feature = "chip-sa1")]
const SA1_CRV: u16 = 0x8400;

/// 4-byte marker the mid-DMA variant copies out of ROM `$018000`.
#[cfg(feature = "chip-sa1")]
const SA1_DMA_MARKER: [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF];

/// Reset routine for the SA-1 harness: forced blank, point the SA-1
/// reset vector (CRV) at `SA1_PROGRAM`, release the SA-1 from reset,
/// enable NMI + display, and idle. The `STA $2200 = $00` write wakes the
/// second CPU (resb was set at power-on and bit 7 is clear => a deferred
/// reset that loads PC = CRV on the next catch-up).
///
/// ```text
/// LDA #$80 / STA $2100        ; forced blank
/// LDA #$00 / STA $2203        ; CRV lo
/// LDA #$84 / STA $2204        ; CRV hi -> CRV = $8400
/// LDA #$00 / STA $2200        ; release SA-1 (wakes at CRV)
/// LDA #$81 / STA $4200        ; NMI + auto-joypad
/// LDA #$0F / STA $2100        ; display on
/// BRA -2                      ; idle
/// ```
#[cfg(feature = "chip-sa1")]
#[rustfmt::skip]
const RESET_SA1: [u8; 32] = [
    0xA9, 0x80, 0x8D, 0x00, 0x21, // LDA #$80 / STA $2100
    0xA9, 0x00, 0x8D, 0x03, 0x22, // LDA #$00 / STA $2203 (CRV lo)
    0xA9, 0x84, 0x8D, 0x04, 0x22, // LDA #$84 / STA $2204 (CRV hi)
    0xA9, 0x00, 0x8D, 0x00, 0x22, // LDA #$00 / STA $2200 (release SA-1)
    0xA9, 0x81, 0x8D, 0x00, 0x42, // LDA #$81 / STA $4200
    0xA9, 0x0F, 0x8D, 0x00, 0x21, // LDA #$0F / STA $2100
    0x80, 0xFE,                   // BRA -2
];

/// Reset routine for the mid-DMA variant: wake the SA-1 exactly as
/// `RESET_SA1` does, then run a normal SA-1 DMA copying 4 bytes from ROM
/// `$018000` (`SA1_DMA_MARKER`) into I-RAM `$20`. The transfer fires the
/// instant `$2236` (DDA middle) is written, so by the first frame the DMA
/// register file *and* the copied I-RAM bytes both hold non-default state
/// worth snapshotting.
///
/// ```text
/// LDA #$80 / STA $2100        ; forced blank
/// LDA #$00 / STA $2203        ; CRV lo
/// LDA #$84 / STA $2204        ; CRV hi -> CRV = $8400
/// LDA #$00 / STA $2200        ; release SA-1
/// LDA #$80 / STA $2230        ; DCNT: dmaen, sd = ROM, dd = I-RAM
/// LDA #$00 / STA $2232        ; SDA lo
/// LDA #$80 / STA $2233        ; SDA mid
/// LDA #$01 / STA $2234        ; SDA hi -> SDA = $018000
/// LDA #$04 / STA $2238        ; DTC lo = 4 bytes
/// LDA #$00 / STA $2239        ; DTC hi
/// LDA #$20 / STA $2235        ; DDA lo = I-RAM $20
/// LDA #$00 / STA $2236        ; DDA mid -> fires the transfer
/// LDA #$81 / STA $4200        ; NMI + auto-joypad
/// LDA #$0F / STA $2100        ; display on
/// BRA -2                      ; idle
/// ```
#[cfg(feature = "chip-sa1")]
#[rustfmt::skip]
const RESET_SA1_DMA: [u8; 72] = [
    0xA9, 0x80, 0x8D, 0x00, 0x21, // forced blank
    0xA9, 0x00, 0x8D, 0x03, 0x22, // CRV lo
    0xA9, 0x84, 0x8D, 0x04, 0x22, // CRV hi
    0xA9, 0x00, 0x8D, 0x00, 0x22, // release SA-1
    0xA9, 0x80, 0x8D, 0x30, 0x22, // DCNT
    0xA9, 0x00, 0x8D, 0x32, 0x22, // SDA lo
    0xA9, 0x80, 0x8D, 0x33, 0x22, // SDA mid
    0xA9, 0x01, 0x8D, 0x34, 0x22, // SDA hi
    0xA9, 0x04, 0x8D, 0x38, 0x22, // DTC lo
    0xA9, 0x00, 0x8D, 0x39, 0x22, // DTC hi
    0xA9, 0x20, 0x8D, 0x35, 0x22, // DDA lo
    0xA9, 0x00, 0x8D, 0x36, 0x22, // DDA mid -> fire
    0xA9, 0x81, 0x8D, 0x00, 0x42, // NMI + auto-joypad
    0xA9, 0x0F, 0x8D, 0x00, 0x21, // display on
    0x80, 0xFE,                   // BRA -2
];

/// 1 MiB LoROM whose header declares an SA-1 cart (map_mode $23,
/// cartridge type $35 -> battery BW-RAM) with 8 KiB BW-RAM. Stamps the
/// reset routine at rom[0], the SA-1 program at rom[$0400], and the DMA
/// source marker at rom[$8000]; the NMI/IRQ handlers sit at rom[$0300] /
/// rom[$0320], clear of the (up-to-72-byte) reset routine. `marker`
/// distinguishes two builds so their SHA-256s differ.
#[cfg(feature = "chip-sa1")]
fn build_sa1_rom(reset_code: &[u8], marker: u8) -> Vec<u8> {
    const SIZE: usize = 0x10_0000;
    let mut chip = vec![0u8; SIZE];
    chip[RESET_BASE..RESET_BASE + reset_code.len()].copy_from_slice(reset_code);
    chip[0x0300] = 0x40; // NMI handler: bare RTI ($8300)
    chip[0x0320] = 0x40; // IRQ handler: bare RTI ($8320)
    chip[0x0100] = marker;
    chip[0x0400..0x0400 + SA1_PROGRAM.len()].copy_from_slice(&SA1_PROGRAM);
    chip[0x8000..0x8000 + SA1_DMA_MARKER.len()].copy_from_slice(&SA1_DMA_MARKER);

    let off = 0x7FB0;
    chip[off + 0x10..off + 0x10 + 21].copy_from_slice(b"SA1 SAVESTATE TEST   ");
    chip[off + 0x25] = 0x23; // map_mode $23 -> SA-1 (LoROM-located)
    chip[off + 0x26] = 0x35; // cartridge_type $35 -> SA-1 + battery BW-RAM
    chip[off + 0x27] = 0x0A; // 1 MiB
    chip[off + 0x28] = 0x03; // 8 KiB BW-RAM (1024 << 3)
    chip[off + 0x29] = 0x01; // USA -> NTSC
    chip[off + 0x2A] = 0x01;
    chip[off + 0x2B] = 0x00;
    let checksum: u16 = 0x1234;
    chip[off + 0x2C..off + 0x2E].copy_from_slice(&(!checksum).to_le_bytes());
    chip[off + 0x2E..off + 0x30].copy_from_slice(&checksum.to_le_bytes());
    chip[0x7FFA..0x7FFC].copy_from_slice(&0x8300u16.to_le_bytes()); // NMI vector
    chip[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes()); // reset vector
    chip[0x7FFE..0x8000].copy_from_slice(&0x8320u16.to_le_bytes()); // IRQ vector
    chip
}

#[cfg(feature = "chip-sa1")]
fn loaded_sa1(reset_code: &[u8], marker: u8) -> SnesEmulator {
    let rom = build_sa1_rom(reset_code, marker);
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom).expect("SA-1 load_rom should succeed");
    assert_eq!(
        emu.preset_kind(),
        Some(PresetKind::Sa1),
        "map_mode $23 + type $35 must detect as SA-1"
    );
    assert_eq!(emu.coprocessor_label(), Some("SA-1"));
    emu
}

/// The SA-1's restorable execution observables: the second CPU's PC and
/// cycle count, read out of the debug state (side-effect-free).
#[cfg(feature = "chip-sa1")]
fn sa1_pc_and_cycles(emu: &SnesEmulator) -> (Option<u32>, u64) {
    let st = emu
        .coprocessor_debug_state()
        .expect("SA-1 cartridge exposes chip state");
    (st.program_counter, st.cycle_count)
}

#[cfg(feature = "chip-sa1")]
#[test]
fn sa1_save_load_resumes_deterministically() {
    let mut emu_a = loaded_sa1(&RESET_SA1, 0x11);
    emu_a.set_audio_enabled(true);
    for _ in 0..3 {
        emu_a.run_frame();
    }
    // The reset routine drove the SA-1 register window and woke the
    // second CPU, which has been running its I-RAM loop.
    assert!(
        emu_a.coprocessor_accesses() > 0,
        "reset routine must touch the SA-1 register window"
    );
    let (pc_a, cyc_a) = sa1_pc_and_cycles(&emu_a);
    assert!(cyc_a > 0, "the SA-1 CPU must have executed instructions");
    assert!(
        pc_a.is_some_and(|pc| pc >= u32::from(SA1_CRV) && pc < 0x1_0000),
        "SA-1 is running inside its ROM window, got {pc_a:?}"
    );

    let snapshot = emu_a.save_state();

    let mut emu_b = loaded_sa1(&RESET_SA1, 0x11);
    emu_b.set_audio_enabled(true);
    emu_b
        .load_state(&snapshot)
        .expect("load_state should succeed");

    // The restore reinstates the second CPU's PC + cycle count at once.
    assert_eq!(
        sa1_pc_and_cycles(&emu_b),
        (pc_a, cyc_a),
        "SA-1 PC + cycle count must survive the savestate"
    );

    // Both must then advance in lockstep.
    for _ in 0..2 {
        emu_a.run_frame();
        emu_b.run_frame();
    }
    assert_eq!(
        observe(&emu_a),
        observe(&emu_b),
        "restored emulator (with SA-1) must resume bit-for-bit"
    );
    assert_eq!(
        sa1_pc_and_cycles(&emu_a),
        sa1_pc_and_cycles(&emu_b),
        "SA-1 must stay in lockstep after restore"
    );
}

#[cfg(feature = "chip-sa1")]
#[test]
fn sa1_restore_is_self_consistent_resave_matches() {
    // If any persisted SA-1 field (second CPU, catch-up anchor, I-RAM,
    // MMC, math, timer, IRQ, DMA, varbit) were dropped on the way through
    // the savestate, the immediate re-save would diverge from the original.
    let mut emu_a = loaded_sa1(&RESET_SA1, 0x22);
    for _ in 0..4 {
        emu_a.run_frame();
    }
    let snapshot = emu_a.save_state();

    let mut emu_b = loaded_sa1(&RESET_SA1, 0x22);
    emu_b
        .load_state(&snapshot)
        .expect("load_state should succeed");
    let resave = emu_b.save_state();

    assert_eq!(
        snapshot, resave,
        "re-saving immediately after an SA-1 restore must reproduce the same bytes"
    );
}

#[cfg(feature = "chip-sa1")]
#[test]
fn sa1_dma_state_round_trips_through_a_savestate() {
    let mut emu_a = loaded_sa1(&RESET_SA1_DMA, 0x33);
    for _ in 0..3 {
        emu_a.run_frame();
    }
    // The reset routine fired a normal SA-1 DMA copying the ROM marker
    // into I-RAM $20 (the second CPU's own loop only touches $10).
    let iram_a = emu_a
        .coprocessor_debug_state()
        .and_then(|s| s.internal_ram)
        .expect("SA-1 exposes I-RAM");
    assert_eq!(
        &iram_a[0x20..0x24],
        &SA1_DMA_MARKER,
        "normal DMA must have copied the ROM marker into I-RAM"
    );

    let snapshot = emu_a.save_state();

    let mut emu_b = loaded_sa1(&RESET_SA1_DMA, 0x33);
    emu_b
        .load_state(&snapshot)
        .expect("load_state should succeed");

    // The DMA'd bytes survive the round-trip.
    let iram_b = emu_b
        .coprocessor_debug_state()
        .and_then(|s| s.internal_ram)
        .expect("SA-1 exposes I-RAM");
    assert_eq!(
        &iram_b[0x20..0x24],
        &SA1_DMA_MARKER,
        "DMA result must survive the savestate"
    );

    // Byte-identical resave proves every SA-1 sub-state (CPU, I-RAM, DMA
    // register file, timer, IRQ) was both captured and restored.
    let resave = emu_b.save_state();
    assert_eq!(
        snapshot, resave,
        "re-save after an SA-1 DMA restore must match"
    );

    // And both advance in lockstep.
    for _ in 0..2 {
        emu_a.run_frame();
        emu_b.run_frame();
    }
    assert_eq!(observe(&emu_a), observe(&emu_b));
}

// -------------------------------------------------------------
// SA-1 as a first-class debugger CPU (slot 2)
// -------------------------------------------------------------

/// An SA-1 cart surfaces the chip's second 65816 as a third debugger CPU
/// slot; a plain cart stays a two-CPU console.
#[cfg(feature = "chip-sa1")]
#[test]
fn sa1_cart_exposes_a_third_cpu_slot() {
    let sa1 = loaded_sa1(&RESET_SA1, 0x44);
    let cpus = &sa1.system_info().cpus;
    assert_eq!(cpus.len(), 3, "SA-1 cart adds the second 65816 as slot 2");
    assert_eq!(cpus[2].id, 2);
    assert_eq!(cpus[2].architecture, retro_core::CpuArchitecture::Mos65816);
    assert_eq!(cpus[2].location, retro_core::CpuLocation::Cartridge);

    // A plain LoROM keeps the standard two CPUs, the extra slot is gated on
    // an installed steppable coprocessor, not the SNES itself.
    let plain = loaded(0, 0x66);
    assert_eq!(
        plain.system_info().cpus.len(),
        2,
        "a plain cart must not grow a third CPU slot"
    );
}

/// After the S-CPU wakes the SA-1, slot 2 is a live steppable/observable
/// debugger CPU: its registers surface in `get_cpu_states`, `step_cpu(2)`
/// advances exactly the second 65816, and `disassemble_cpu(2, ...)` decodes
/// its program, all without disturbing the 5A22 (slot 0).
#[cfg(feature = "chip-sa1")]
#[test]
fn sa1_slot_two_steps_and_disassembles() {
    let mut emu = loaded_sa1(&RESET_SA1, 0x55);
    // Run a few frames so the reset routine wakes the SA-1 and it starts
    // looping in its ROM window.
    for _ in 0..3 {
        emu.run_frame();
    }

    // Focus can move to the SA-1 slot.
    emu.set_focused_cpu(2);
    assert_eq!(emu.focused_cpu(), 2, "slot 2 is focusable on an SA-1 cart");

    // The cached CPU-state vector is only refreshed on pause/step, so read the
    // SA-1's live PC from the chip's debug state for disassembly.
    let (sa1_pc_opt, cyc_before) = sa1_pc_and_cycles(&emu);
    let sa1_pc = sa1_pc_opt.expect("SA-1 exposes a PC");
    assert!(
        sa1_pc >= u32::from(SA1_CRV) && sa1_pc < 0x1_0000,
        "SA-1 PC is inside its ROM window, got {sa1_pc:#06x}"
    );

    // Disassembling slot 2 decodes real instructions at the SA-1's PC.
    let rows = emu.disassemble_cpu(2, sa1_pc, 3);
    assert_eq!(rows.len(), 3, "three SA-1 instructions decode");
    assert_eq!(rows[0].address, sa1_pc);
    assert!(
        rows.iter().all(|r| !r.mnemonic.is_empty()),
        "every decoded SA-1 row has a mnemonic, got {rows:?}"
    );

    // Stepping slot 2 advances the SA-1 without touching the 5A22 (slot 0).
    let pc0_before = ((emu.cpu().pbr as u32) << 16) | emu.cpu().pc as u32;
    let step = emu.step_cpu(2);
    assert!(step.instruction_executed, "the SA-1 single-stepped");
    assert!(step.cycles > 0, "the step charged master clocks");
    let cyc_after = sa1_pc_and_cycles(&emu).1;
    assert!(cyc_after > cyc_before, "SA-1 cycle count advanced");
    let pc0_after = ((emu.cpu().pbr as u32) << 16) | emu.cpu().pc as u32;
    assert_eq!(
        pc0_after, pc0_before,
        "stepping the SA-1 must not advance the 5A22 (slot 0)"
    );

    // `step_cpu` refreshed the cached CPU-state vector: three slots, with the
    // SA-1 stamped as its real slot id (not the chip's raw 0).
    let states = emu.debugger().get_cpu_states();
    assert_eq!(states.len(), 3, "three CPU snapshots");
    assert_eq!(states[2].cpu_id, 2, "slot 2 is stamped with its real id");
}

// -------------------------------------------------------------
// Super FX coprocessor savestate (synthetic LoROM Super FX, map_mode $20)
// -------------------------------------------------------------

/// S-CPU reset routine that starts the GSU and idles, no `CLI`, so the
/// (unmasked) STOP IRQ latches but is never serviced (the loop stays
/// inside `BRA -2`), which keeps the harness deterministic without an
/// acknowledging handler.
///
/// ```text
/// LDA #$00 / STA $301E        ; R15 low  = $00
/// LDA #$01 / STA $301F        ; R15 high = $01 -> R15 = $0100, sets GO
/// BRA -2                      ; idle
/// ```
#[cfg(feature = "chip-superfx")]
#[rustfmt::skip]
const RESET_START_GSU: [u8; 12] = [
    0xA9, 0x00, 0x8D, 0x1E, 0x30, // LDA #$00 / STA $301E (R15 lo)
    0xA9, 0x01, 0x8D, 0x1F, 0x30, // LDA #$01 / STA $301F (R15 hi -> GO)
    0x80, 0xFE,                   // BRA -2
];

/// GSU kernel: `inc r0 ; bra -3 ; nop`. A perpetual loop that keeps GO set
/// and evolves `R0`, the PC, and the cycle count every iteration, so a
/// mid-run snapshot must carry a *live* GSU that resumes in lockstep.
#[cfg(feature = "chip-superfx")]
#[rustfmt::skip]
const GSU_LOOP_KERNEL: [u8; 4] = [0xd0, 0x05, 0xFD, 0x01];

/// GSU kernel: `iwt r1,#$BEEF ; alt2 ; sm ($0010),r1 ; stop`. Writes a word
/// into (volatile, no-battery) Game Pak RAM and halts.
#[cfg(feature = "chip-superfx")]
#[rustfmt::skip]
const GSU_WRITE_RAM_KERNEL: [u8; 8] = [0xf1, 0xef, 0xbe, 0x3e, 0xf1, 0x10, 0x00, 0x00];

/// 1 MiB Super FX LoROM (map_mode $20, cartridge type $13 -> Star Fox's
/// 32 KiB volatile Game Pak RAM). Stamps the reset routine at rom[0], the
/// GSU kernel at rom[$0100] (reached at `PBR = 0`, `R15 = $0100`), and a
/// `marker` past the header so two builds hash differently.
#[cfg(feature = "chip-superfx")]
fn build_superfx_rom(reset_code: &[u8], gsu_kernel: &[u8], marker: u8) -> Vec<u8> {
    const SIZE: usize = 0x10_0000;
    let mut chip = vec![0u8; SIZE];
    chip[RESET_BASE..RESET_BASE + reset_code.len()].copy_from_slice(reset_code);
    chip[NMI_HANDLER_OFF] = 0x40; // NMI handler: bare RTI
    chip[0x0060] = 0x40; // IRQ handler: bare RTI (never serviced, I stays set)
    chip[0x0100..0x0100 + gsu_kernel.len()].copy_from_slice(gsu_kernel);
    chip[0x0200] = marker;

    let off = 0x7FB0;
    chip[off + 0x10..off + 0x10 + 21].copy_from_slice(b"SUPERFX SAVESTATE TST");
    chip[off + 0x25] = 0x20; // map_mode $20 -> LoROM (Super FX shares it)
    chip[off + 0x26] = 0x13; // cartridge_type $13 -> Super FX (Star Fox, 32 KiB scratch)
    chip[off + 0x27] = 0x0A; // 1 MiB
    chip[off + 0x28] = 0x00; // no standard SRAM (GSU RAM is derived)
    chip[off + 0x29] = 0x01; // USA -> NTSC
    chip[off + 0x2A] = 0x01;
    chip[off + 0x2B] = 0x00;
    let checksum: u16 = 0x1234;
    chip[off + 0x2C..off + 0x2E].copy_from_slice(&(!checksum).to_le_bytes());
    chip[off + 0x2E..off + 0x30].copy_from_slice(&checksum.to_le_bytes());
    chip[0x7FFA..0x7FFC].copy_from_slice(&NMI_HANDLER_ADDR.to_le_bytes());
    chip[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes());
    chip[0x7FFE..0x8000].copy_from_slice(&IRQ_HANDLER_ADDR.to_le_bytes());
    chip
}

#[cfg(feature = "chip-superfx")]
fn loaded_superfx(reset_code: &[u8], gsu_kernel: &[u8], marker: u8) -> SnesEmulator {
    let rom = build_superfx_rom(reset_code, gsu_kernel, marker);
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom)
        .expect("Super FX load_rom should succeed");
    assert_eq!(
        emu.preset_kind(),
        Some(PresetKind::SuperFx),
        "map_mode $20 + type $13 must detect as Super FX"
    );
    assert_eq!(emu.coprocessor_label(), Some("Super FX"));
    emu
}

/// The GSU's restorable execution observables: PC + cycle count, read out
/// of the side-effect-free debug state.
#[cfg(feature = "chip-superfx")]
fn gsu_pc_and_cycles(emu: &SnesEmulator) -> (Option<u32>, u64) {
    let st = emu
        .coprocessor_debug_state()
        .expect("Super FX cartridge exposes chip state");
    (st.program_counter, st.cycle_count)
}

#[cfg(feature = "chip-superfx")]
#[test]
fn superfx_save_load_resumes_deterministically() {
    let mut emu_a = loaded_superfx(&RESET_START_GSU, &GSU_LOOP_KERNEL, 0x11);
    emu_a.set_audio_enabled(true);
    for _ in 0..3 {
        emu_a.run_frame();
    }
    // The reset routine armed R15 + GO through the GSU register window and the
    // per-instruction burst hook has been running the loop.
    assert!(
        emu_a.coprocessor_accesses() > 0,
        "reset routine must touch the GSU register window"
    );
    let (pc_a, cyc_a) = gsu_pc_and_cycles(&emu_a);
    assert!(cyc_a > 0, "the GSU must have executed instructions");

    let snapshot = emu_a.save_state();

    let mut emu_b = loaded_superfx(&RESET_START_GSU, &GSU_LOOP_KERNEL, 0x11);
    emu_b.set_audio_enabled(true);
    emu_b
        .load_state(&snapshot)
        .expect("load_state should succeed");

    // The restore reinstates the GSU's PC + cycle count at once.
    assert_eq!(
        gsu_pc_and_cycles(&emu_b),
        (pc_a, cyc_a),
        "GSU PC + cycle count must survive the savestate"
    );

    // Both must then advance in lockstep.
    for _ in 0..2 {
        emu_a.run_frame();
        emu_b.run_frame();
    }
    assert_eq!(
        observe(&emu_a),
        observe(&emu_b),
        "restored emulator (with Super FX) must resume bit-for-bit"
    );
    assert_eq!(
        gsu_pc_and_cycles(&emu_a),
        gsu_pc_and_cycles(&emu_b),
        "GSU must stay in lockstep after restore"
    );
}

#[cfg(feature = "chip-superfx")]
#[test]
fn superfx_restore_is_self_consistent_resave_matches() {
    // If any persisted GSU field (register file, PC, banks, cache, pixel
    // caches, ROM/RAM buffers, catch-up anchor, Game Pak RAM) were dropped on
    // the way through the savestate, the immediate re-save would diverge.
    let mut emu_a = loaded_superfx(&RESET_START_GSU, &GSU_LOOP_KERNEL, 0x22);
    for _ in 0..4 {
        emu_a.run_frame();
    }
    let snapshot = emu_a.save_state();

    let mut emu_b = loaded_superfx(&RESET_START_GSU, &GSU_LOOP_KERNEL, 0x22);
    emu_b
        .load_state(&snapshot)
        .expect("load_state should succeed");
    let resave = emu_b.save_state();

    assert_eq!(
        snapshot, resave,
        "re-saving immediately after a Super FX restore must reproduce the same bytes"
    );
}

#[cfg(feature = "chip-superfx")]
#[test]
fn superfx_volatile_game_pak_ram_round_trips_through_savestate() {
    // The GSU writes $BEEF into its volatile (no-battery) Game Pak RAM. That
    // RAM lives ONLY in the savestate (never the `.srm`), so a round-trip that
    // preserves it proves the machine-complete RAM capture end-to-end through
    // the full SnesEmulator envelope.
    let mut emu_a = loaded_superfx(&RESET_START_GSU, &GSU_WRITE_RAM_KERNEL, 0x33);
    for _ in 0..3 {
        emu_a.run_frame();
    }
    let ram_a = emu_a
        .coprocessor_debug_state()
        .and_then(|s| s.internal_ram)
        .expect("Super FX exposes Game Pak RAM");
    assert_eq!(
        &ram_a[0x10..0x12],
        &[0xEF, 0xBE],
        "GSU stored $BEEF into Game Pak RAM $0010"
    );

    let snapshot = emu_a.save_state();

    let mut emu_b = loaded_superfx(&RESET_START_GSU, &GSU_WRITE_RAM_KERNEL, 0x33);
    emu_b
        .load_state(&snapshot)
        .expect("load_state should succeed");
    let ram_b = emu_b
        .coprocessor_debug_state()
        .and_then(|s| s.internal_ram)
        .expect("Super FX exposes Game Pak RAM");
    assert_eq!(
        &ram_b[0x10..0x12],
        &[0xEF, 0xBE],
        "volatile Game Pak RAM survived the savestate (no .srm)"
    );
}
