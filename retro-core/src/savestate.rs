//! Cross-core savestate envelope: versioned, identified, integrity-checked.
//!
//! Each console core (sg1, snes1) defines its own
//! `SystemSaveState` struct with whatever component snapshots it needs.
//! Hand that struct to [`serialize_compressed`] to get back bytes the
//! host can persist (file, OPFS, slot in a DB...). [`deserialize_compressed`]
//! reverses the pipeline and gives back the typed payload plus
//! metadata (version, core_id, rom_hash, created_at).
//!
//! On-disk layout:
//!
//! ```text
//! [MAGIC (4 bytes: b"RLSS")] [zstd(bincode(envelope))]
//! ```
//!
//! The 4-byte magic prefix lets callers distinguish retro-core
//! savestates from legacy formats (e.g. SG1 v1/v2 savestates that
//! pre-date this envelope) via [`has_magic`]. SG1 uses that to fall
//! back to its old reader when loading historical files.
//!
//! Pipeline:
//! 1. `bincode::serialize(envelope)` -> compact, schema-locked bytes.
//! 2. `zstd::encode_all(level)` -> compressible payloads (mostly zero
//!    RAM / VRAM) shrink ~10-50x.
//! 3. Prepend MAGIC.
//!
//! Reverse on read; [`deserialize_compressed`] errors clearly when
//! magic is missing, so callers can decide on a fallback path.
//!
//! ROM hash verification is opt-in via [`check_rom_hash`] (and its
//! string-error form [`verify_rom_hash`]): an empty `rom_hash` field
//! acts as a wildcard (matches anything) so legacy savestates that never
//! embedded a hash still load. Otherwise a mismatch surfaces as
//! [`LoadStateError::OtherRom`], naming both hashes, unless the host
//! passed [`RomCheck::AnyRom`] because the player chose to load a state
//! made on another build of the game.

use std::fmt;

use serde::{Deserialize, Serialize};

/// 4-byte sentinel at the start of every retro-core savestate.
///
/// Stands for "RetroLab SaveState". Used by [`has_magic`] /
/// [`deserialize_compressed`] to tell envelope-format bytes from a
/// legacy core-specific format that might land in the same slot.
pub const RETRO_SAVESTATE_MAGIC: &[u8; 4] = b"RLSS";

/// Version of the envelope wire format itself.
///
/// Bumped only when [`SaveStateEnvelope`]'s on-disk shape changes
/// (new metadata fields, magic relayout, compression swap...). The
/// `core_version` field is the *payload's* version, that one belongs
/// to whatever `T` the core defines.
pub const RETRO_SAVESTATE_ENVELOPE_VERSION: u32 = 1;

/// Versioned, identified wrapper around a per-core savestate payload.
///
/// Type parameter `T` is the core's own state struct (e.g. SG1's
/// `SystemSaveState`). Keep `T` `Serialize + DeserializeOwned`; that's
/// the only constraint the envelope enforces.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SaveStateEnvelope<T> {
    /// Always equals [`RETRO_SAVESTATE_ENVELOPE_VERSION`]. Reserved for
    /// future on-disk format migrations.
    pub envelope_version: u32,

    /// Short identifier of the core that produced this savestate, e.g.
    /// `"sg1"`, `"snes1"`. A host can read it to refuse a cross-core
    /// load.
    pub core_id: String,

    /// The core's own payload version: SG1's `SAVE_STATE_VERSION`,
    /// SNES1's `SNES_SAVESTATE_VERSION`.
    pub core_version: u32,

    /// SHA-256 (hex) of the ROM the savestate was captured against.
    /// Empty string means "unknown" and effectively disables the
    /// integrity check (see [`verify_rom_hash`]).
    pub rom_hash: String,

    /// Unix timestamp (seconds since epoch) at capture time. Set by
    /// the host; the envelope only persists it.
    pub created_at: u64,

    /// The core-specific snapshot payload.
    pub payload: T,
}

/// `true` when `bytes` starts with [`RETRO_SAVESTATE_MAGIC`]. Cheap
/// (4-byte compare); call before [`deserialize_compressed`] when you
/// need to gracefully fall back to a legacy reader.
#[inline]
pub fn has_magic(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && &bytes[..4] == RETRO_SAVESTATE_MAGIC
}

/// Pipeline: bincode -> zstd at `level` -> prepend MAGIC.
///
/// `level` follows zstd convention: 1 is fast, 3 is the default for
/// SG1 (good ratio, ~1 ms on a 200 KiB payload), up to 22 for max
/// compression at considerable CPU cost.
pub fn serialize_compressed<T: Serialize>(
    env: &SaveStateEnvelope<T>,
    level: i32,
) -> Result<Vec<u8>, String> {
    let serialized =
        bincode::serialize(env).map_err(|e| format!("savestate serialize failed: {e}"))?;
    let compressed = zstd::encode_all(&serialized[..], level)
        .map_err(|e| format!("savestate zstd encode failed: {e}"))?;
    let mut out = Vec::with_capacity(RETRO_SAVESTATE_MAGIC.len() + compressed.len());
    out.extend_from_slice(RETRO_SAVESTATE_MAGIC);
    out.extend_from_slice(&compressed);
    Ok(out)
}

/// Reverse of [`serialize_compressed`].
///
/// Errors:
/// - `"not a retro-core savestate (missing magic)"` when the magic
///   prefix is absent. Caller is expected to handle this (fall back
///   to a legacy reader, surface to the user, etc.).
/// - Bubble-ups from zstd / bincode otherwise.
pub fn deserialize_compressed<T>(bytes: &[u8]) -> Result<SaveStateEnvelope<T>, String>
where
    T: for<'de> Deserialize<'de>,
{
    if !has_magic(bytes) {
        return Err("not a retro-core savestate (missing magic)".to_string());
    }
    let payload = &bytes[RETRO_SAVESTATE_MAGIC.len()..];
    let decompressed =
        zstd::decode_all(payload).map_err(|e| format!("savestate zstd decode failed: {e}"))?;
    let env: SaveStateEnvelope<T> = bincode::deserialize(&decompressed)
        .map_err(|e| format!("savestate deserialize failed: {e}"))?;
    Ok(env)
}

/// Whether a savestate must have been captured against the ROM it is
/// loaded into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RomCheck {
    /// Refuse a savestate made on another ROM: what every load does
    /// unless the player chose otherwise.
    SameRom,
    /// Load it whatever ROM it was made on. For a host that asked the
    /// player first, typically to carry a state from one build of a
    /// game (a translation in progress, a hack) to the next.
    AnyRom,
}

/// Why a core refused to load a savestate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadStateError {
    /// The savestate was captured against another ROM. Each field names
    /// a ROM the way the savestate does: its SHA-256 in hex, or the game
    /// title in an SG1 savestate older than the envelope.
    OtherRom {
        state_rom: String,
        loaded_rom: String,
    },
    /// Anything else: not a savestate, a corrupt one, another core's, or
    /// a layout newer than this core reads.
    Unreadable(String),
}

impl fmt::Display for LoadStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OtherRom {
                state_rom,
                loaded_rom,
            } => write!(
                f,
                "ROM hash mismatch: savestate is for '{state_rom}' but loaded ROM is '{loaded_rom}'"
            ),
            Self::Unreadable(reason) => f.write_str(reason),
        }
    }
}

impl From<String> for LoadStateError {
    fn from(reason: String) -> Self {
        Self::Unreadable(reason)
    }
}

/// Refuse to load a savestate whose ROM hash doesn't match `expected`,
/// unless `check` is [`RomCheck::AnyRom`].
///
/// Wildcard rule: if `env.rom_hash` is empty (the legacy "we don't
/// know" sentinel) the check passes. This lets old savestates that
/// pre-date hash embedding still load, the user accepts the risk
/// implicitly by virtue of the empty hash.
pub fn check_rom_hash<T>(
    env: &SaveStateEnvelope<T>,
    expected: &str,
    check: RomCheck,
) -> Result<(), LoadStateError> {
    if check == RomCheck::AnyRom || env.rom_hash.is_empty() || env.rom_hash == expected {
        Ok(())
    } else {
        Err(LoadStateError::OtherRom {
            state_rom: env.rom_hash.clone(),
            loaded_rom: expected.to_string(),
        })
    }
}

/// [`check_rom_hash`] with [`RomCheck::SameRom`], the refusal as text.
pub fn verify_rom_hash<T>(env: &SaveStateEnvelope<T>, expected: &str) -> Result<(), String> {
    check_rom_hash(env, expected, RomCheck::SameRom).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
    struct DummyPayload {
        cycles: u64,
        ram: Vec<u8>,
        title: String,
    }

    fn dummy(cycles: u64) -> DummyPayload {
        DummyPayload {
            cycles,
            ram: vec![0u8; 1024],
            title: "Sonic the Hedgehog".to_string(),
        }
    }

    fn envelope(payload: DummyPayload) -> SaveStateEnvelope<DummyPayload> {
        SaveStateEnvelope {
            envelope_version: RETRO_SAVESTATE_ENVELOPE_VERSION,
            core_id: "sg1".to_string(),
            core_version: 2,
            rom_hash: "abc123".to_string(),
            created_at: 1716624000,
            payload,
        }
    }

    #[test]
    fn roundtrip_preserves_payload() {
        let env = envelope(dummy(42));
        let bytes = serialize_compressed(&env, 3).unwrap();
        let back: SaveStateEnvelope<DummyPayload> = deserialize_compressed(&bytes).unwrap();
        assert_eq!(back.envelope_version, env.envelope_version);
        assert_eq!(back.core_id, env.core_id);
        assert_eq!(back.core_version, env.core_version);
        assert_eq!(back.rom_hash, env.rom_hash);
        assert_eq!(back.created_at, env.created_at);
        assert_eq!(back.payload, env.payload);
    }

    #[test]
    fn magic_bytes_present() {
        let env = envelope(dummy(7));
        let bytes = serialize_compressed(&env, 3).unwrap();
        assert!(has_magic(&bytes));
        assert_eq!(&bytes[..4], RETRO_SAVESTATE_MAGIC);
    }

    #[test]
    fn deserialize_without_magic_errors_clearly() {
        // Compressed payload but no magic prefix -> expected error.
        let env = envelope(dummy(1));
        let no_magic = bincode::serialize(&env).unwrap();
        let err = deserialize_compressed::<DummyPayload>(&no_magic).unwrap_err();
        assert!(err.contains("missing magic"), "unexpected error: {err}");
    }

    #[test]
    fn deserialize_truncated_input_errors() {
        // Two bytes only, short of the 4-byte magic.
        let err = deserialize_compressed::<DummyPayload>(&[0xAA, 0xBB]).unwrap_err();
        assert!(err.contains("missing magic"), "unexpected error: {err}");
    }

    #[test]
    fn verify_rom_hash_matches() {
        let env = envelope(dummy(0));
        verify_rom_hash(&env, "abc123").unwrap();
    }

    #[test]
    fn verify_rom_hash_mismatches() {
        let env = envelope(dummy(0));
        let err = verify_rom_hash(&env, "deadbeef").unwrap_err();
        assert!(err.contains("ROM hash mismatch"));
        assert!(err.contains("abc123"));
        assert!(err.contains("deadbeef"));
    }

    #[test]
    fn verify_rom_hash_empty_is_wildcard() {
        // Legacy savestates without hash embedding must still load.
        let mut env = envelope(dummy(0));
        env.rom_hash.clear();
        verify_rom_hash(&env, "anything").unwrap();
        verify_rom_hash(&env, "").unwrap();
    }

    #[test]
    fn check_rom_hash_names_both_roms_when_they_differ() {
        let env = envelope(dummy(0));
        let err = check_rom_hash(&env, "deadbeef", RomCheck::SameRom).unwrap_err();
        assert_eq!(
            err,
            LoadStateError::OtherRom {
                state_rom: "abc123".to_string(),
                loaded_rom: "deadbeef".to_string(),
            }
        );
        // The text is the one `verify_rom_hash` always gave.
        assert_eq!(
            err.to_string(),
            verify_rom_hash(&env, "deadbeef").unwrap_err()
        );
    }

    #[test]
    fn check_rom_hash_any_rom_loads_a_state_made_on_another_rom() {
        let env = envelope(dummy(0));
        check_rom_hash(&env, "deadbeef", RomCheck::AnyRom).unwrap();
        check_rom_hash(&env, "abc123", RomCheck::AnyRom).unwrap();
        check_rom_hash(&env, "abc123", RomCheck::SameRom).unwrap();
    }

    #[test]
    fn unreadable_carries_its_reason_as_text() {
        let err = LoadStateError::from("savestate zstd decode failed".to_string());
        assert_eq!(
            err,
            LoadStateError::Unreadable("savestate zstd decode failed".to_string())
        );
        assert_eq!(err.to_string(), "savestate zstd decode failed");
    }

    #[test]
    fn compression_actually_compresses() {
        // Highly compressible payload (1 KiB of zeros) should be
        // dramatically smaller post-zstd, dominated by the bincode
        // header + envelope metadata.
        let env = envelope(dummy(0));
        let bytes = serialize_compressed(&env, 3).unwrap();
        // Raw bincode size of the envelope (no compression).
        let raw_size = bincode::serialized_size(&env).unwrap() as usize;
        assert!(
            bytes.len() < raw_size,
            "expected compressed ({}) < raw ({})",
            bytes.len(),
            raw_size
        );
    }

    #[test]
    fn envelope_version_round_trip() {
        // Use a non-default version to make sure the field actually
        // flows through bincode (vs. silently being skipped).
        let mut env = envelope(dummy(0));
        env.envelope_version = 99;
        let bytes = serialize_compressed(&env, 3).unwrap();
        let back: SaveStateEnvelope<DummyPayload> = deserialize_compressed(&bytes).unwrap();
        assert_eq!(back.envelope_version, 99);
    }

    #[test]
    fn different_payload_type_serializes_independently() {
        // The envelope is truly generic: two distinct payload types
        // round-trip without interference.
        #[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
        struct OtherPayload {
            channels: [u16; 4],
        }
        let env = SaveStateEnvelope {
            envelope_version: 1,
            core_id: "snes1".to_string(),
            core_version: 1,
            rom_hash: String::new(),
            created_at: 0,
            payload: OtherPayload {
                channels: [1, 2, 3, 4],
            },
        };
        let bytes = serialize_compressed(&env, 3).unwrap();
        let back: SaveStateEnvelope<OtherPayload> = deserialize_compressed(&bytes).unwrap();
        assert_eq!(back.payload.channels, [1, 2, 3, 4]);
        assert_eq!(back.core_id, "snes1");
    }
}
