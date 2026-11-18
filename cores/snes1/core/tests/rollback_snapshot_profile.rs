//! Profiling harness for the rollback-ring snapshot path, on the SNES.
//!
//! The netplay rollback ring re-saves a full machine snapshot on every
//! confirmed frame and re-loads + re-saves on every mis-prediction, so the
//! per-save cost sits directly on the frame budget (warn 8 ms / bad 16 ms).
//! This measures the *actual* `save_state()` serialization cost at the current
//! zstd level 3 against two cheaper candidates, zstd level 1 and uncompressed
//! bincode, across a best case (zero-filled buffers, an idle machine) and a
//! worst case (pseudo-random buffers that defeat zstd entirely, an upper bound
//! no real game reaches).
//!
//! It is `#[ignore]`d because it prints timing rather than asserting: run it
//! explicitly to measure whether the ring needs a faster snapshot format than
//! the portable, compressed savestate:
//!
//! ```bash
//! cargo test --package snes1-core --test rollback_snapshot_profile -- --ignored --nocapture
//! ```
//!
//! Self-contained on purpose: it builds its payload straight from a fresh
//! `SnesEmulator` (no ROM, no shared test helpers) so the file stays
//! clippy-clean and the measurement isolates the serialization pipeline the
//! ring actually exercises.

use std::time::Instant;

use retro_core::savestate::{
    deserialize_compressed, serialize_compressed, SaveStateEnvelope,
    RETRO_SAVESTATE_ENVELOPE_VERSION,
};
use retro_core::EmulatorCore;
use snes1_core::{SnesEmulator, SnesSaveState};

const ITERATIONS: usize = 200;

/// A deterministic, low-cost PRNG (xorshift64) used to fill the big state
/// buffers with incompressible noise, the pathological worst case for zstd.
fn fill_noise(buf: &mut [u8], mut seed: u64) {
    for byte in buf.iter_mut() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        *byte = (seed & 0xff) as u8;
    }
}

fn envelope_of(payload: SnesSaveState) -> SaveStateEnvelope<SnesSaveState> {
    SaveStateEnvelope {
        envelope_version: RETRO_SAVESTATE_ENVELOPE_VERSION,
        core_id: "snes1".to_string(),
        core_version: payload.version,
        rom_hash: String::new(),
        created_at: 0,
        payload,
    }
}

fn percentiles(mut samples: Vec<f64>) -> (f64, f64, f64) {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pick = |q: f64| {
        let idx = ((samples.len() as f64 - 1.0) * q).round() as usize;
        samples[idx]
    };
    (pick(0.50), pick(0.95), samples[samples.len() - 1])
}

/// Time `serialize_compressed(level)` over `ITERATIONS` and report p50/p95/max
/// plus the encoded size.
fn profile_save(label: &str, env: &SaveStateEnvelope<SnesSaveState>, level: i32) {
    let mut times = Vec::with_capacity(ITERATIONS);
    let mut bytes = 0usize;
    for _ in 0..ITERATIONS {
        let start = Instant::now();
        let out = serialize_compressed(env, level).expect("serialize");
        times.push(start.elapsed().as_secs_f64() * 1000.0);
        bytes = out.len();
    }
    let (p50, p95, max) = percentiles(times);
    println!(
        "  save  {label:<22} p50={p50:6.3}ms  p95={p95:6.3}ms  max={max:6.3}ms  bytes={bytes}"
    );
}

/// Time the uncompressed candidate: `bincode::serialize` of the envelope, with no zstd.
fn profile_save_bincode(env: &SaveStateEnvelope<SnesSaveState>) {
    let mut times = Vec::with_capacity(ITERATIONS);
    let mut bytes = 0usize;
    for _ in 0..ITERATIONS {
        let start = Instant::now();
        let out = bincode::serialize(env).expect("bincode serialize");
        times.push(start.elapsed().as_secs_f64() * 1000.0);
        bytes = out.len();
    }
    let (p50, p95, max) = percentiles(times);
    println!(
        "  save  {:<22} p50={p50:6.3}ms  p95={p95:6.3}ms  max={max:6.3}ms  bytes={bytes}",
        "bincode (uncompressed)",
    );
}

fn profile_load(label: &str, env: &SaveStateEnvelope<SnesSaveState>, level: i32) {
    let encoded = serialize_compressed(env, level).expect("serialize");
    let mut times = Vec::with_capacity(ITERATIONS);
    for _ in 0..ITERATIONS {
        let start = Instant::now();
        let back = deserialize_compressed::<SnesSaveState>(&encoded).expect("deserialize");
        times.push(start.elapsed().as_secs_f64() * 1000.0);
        std::hint::black_box(&back);
    }
    let (p50, p95, max) = percentiles(times);
    println!("  load  {label:<22} p50={p50:6.3}ms  p95={p95:6.3}ms  max={max:6.3}ms");
}

fn profile_load_bincode(env: &SaveStateEnvelope<SnesSaveState>) {
    let encoded = bincode::serialize(env).expect("bincode serialize");
    let mut times = Vec::with_capacity(ITERATIONS);
    for _ in 0..ITERATIONS {
        let start = Instant::now();
        let back: SaveStateEnvelope<SnesSaveState> =
            bincode::deserialize(&encoded).expect("bincode deserialize");
        times.push(start.elapsed().as_secs_f64() * 1000.0);
        std::hint::black_box(&back);
    }
    let (p50, p95, max) = percentiles(times);
    println!(
        "  load  {:<22} p50={p50:6.3}ms  p95={p95:6.3}ms  max={max:6.3}ms",
        "bincode (uncompressed)",
    );
}

/// A full-size snapshot payload. `save_state()` works on a fresh emulator
/// (no cartridge), and the fixed-size work RAM / VRAM / ARAM buffers, which
/// dominate the payload, are already allocated by `new`.
fn base_payload() -> SnesSaveState {
    let mut emulator = SnesEmulator::new();
    let bytes = emulator.save_state();
    deserialize_compressed::<SnesSaveState>(&bytes)
        .expect("save_state must round-trip")
        .payload
}

#[test]
#[ignore = "profiling harness: run with --ignored --nocapture to print the timings"]
fn profile_rollback_snapshot_path() {
    let base = base_payload();
    println!("\n=== Rollback snapshot path profile ({ITERATIONS} iterations) ===");
    println!(
        "WRAM={}  VRAM={} words  ARAM={}  (fixed-size buffers dominate the payload)",
        base.bus.wram.len(),
        base.bus.ppu.vram.len(),
        base.bus.apu.aram.len(),
    );

    // Best case: an idle machine (zero-filled buffers). zstd crushes this, so
    // it is the low end of the range.
    println!("\n[best case: idle / zero-filled buffers]");
    let idle = envelope_of(base_payload());
    profile_save("zstd-3 (current)", &idle, 3);
    profile_save("zstd-1", &idle, 1);
    profile_save_bincode(&idle);
    profile_load("zstd-3 (current)", &idle, 3);
    profile_load_bincode(&idle);

    // Worst case: incompressible noise in every big buffer. No real SNES
    // frame reaches this, but it bounds the compression cost from above.
    println!("\n[worst case: incompressible / noise-filled buffers]");
    let mut noisy_payload = base_payload();
    fill_noise(&mut noisy_payload.bus.wram, 0x1234_5678_9abc_def0);
    fill_noise(&mut noisy_payload.bus.apu.aram, 0x0fed_cba9_8765_4321);
    let mut vram = vec![0u8; noisy_payload.bus.ppu.vram.len() * 2];
    fill_noise(&mut vram, 0xdead_beef_cafe_babe);
    for (word, pair) in noisy_payload.bus.ppu.vram.iter_mut().zip(vram.chunks(2)) {
        *word = u16::from_le_bytes([pair[0], pair[1]]);
    }
    let noisy = envelope_of(noisy_payload);
    profile_save("zstd-3 (current)", &noisy, 3);
    profile_save("zstd-1", &noisy, 1);
    profile_save_bincode(&noisy);
    profile_load("zstd-3 (current)", &noisy, 3);
    profile_load_bincode(&noisy);

    println!("\n=== end profile ===\n");
}
