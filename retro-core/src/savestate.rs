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
//! A core seals and opens its savestates with [`seal`] and [`open`]
//! over its payload type, a [`SavestatePayload`]: `open` refuses another
//! core's state, a layout or an envelope newer than the build reads, and,
//! unless the host passed [`RomCheck::AnyRom`] because the player chose to
//! load a state made on another build of the game, a state made on another
//! ROM ([`LoadStateError::OtherRom`], naming both). An empty `rom_hash` is
//! a wildcard, so a state that never named its ROM still loads.
//!
//! After the payload a state may hold an extension
//! ([`SavestateExtension`]): what the machine held that the payload's
//! layout has no field for, written only when it holds something. Every
//! reader of the envelope stops at the payload's end, so a build older
//! than an extension loads the state without it.

use std::fmt;

use serde::de::DeserializeOwned;
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
    /// integrity check (see [`open`]).
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
fn check_rom_hash<T>(
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

/// A console's savestate format: the core id its envelopes carry, and the
/// newest layout of its payload this build reads and writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SavestateFormat {
    pub core_id: &'static str,
    pub version: u32,
}

/// The cartridge a machine holds, as a savestate names it.
#[derive(Clone, Copy, Debug)]
pub struct RomIdentity<'a> {
    /// The image's SHA-256 in hex, which an envelope names its ROM by.
    pub sha256: &'a str,
    /// The title, which a state older than the envelope names its ROM by.
    pub title: &'a str,
}

/// What a payload is built for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayloadUse {
    /// A savestate: the console may stamp the time and the title.
    Savestate,
    /// The state hash: nothing but the machine's state.
    StateHash,
}

/// A console's savestate payload. Its bincode layout never moves: a field
/// added, removed or moved makes every state written before unreadable.
pub trait SavestatePayload: Serialize + DeserializeOwned {
    /// The core id and the newest layout version this build reads.
    const FORMAT: SavestateFormat;
    /// What a state of this console holds after its payload.
    type Extension: SavestateExtension;
    /// Refuse a payload whose fields cannot be restored (a newer layout, a
    /// memory of the wrong size).
    fn validate(&self) -> Result<(), String>;
    /// Named FNV-1a-64 hashes of the payload's parts, for finding where two
    /// machines that should be equal differ.
    fn hash_parts(&self) -> Vec<(&'static str, u64)>;
    /// Bytes without the magic: a console that wrote another form before
    /// the envelope reads it here. The others refuse them.
    fn open_without_envelope(
        bytes: &[u8],
        rom: Option<RomIdentity<'_>>,
        check: RomCheck,
    ) -> Result<Self, LoadStateError> {
        let _ = (bytes, rom, check);
        Err(LoadStateError::Unreadable(
            "not a retro-core savestate (missing magic)".to_string(),
        ))
    }
    /// The payload and `extension` sealed ([`seal`]). Written by
    /// [`savestate_codec!`](crate::savestate_codec).
    fn seal(
        &self,
        extension: &Self::Extension,
        rom_sha256: &str,
        created_at: u64,
    ) -> Result<Vec<u8>, String>;
    /// A sealed state opened ([`open`]). Written by
    /// [`savestate_codec!`](crate::savestate_codec).
    fn open(
        bytes: &[u8],
        rom: Option<RomIdentity<'_>>,
        check: RomCheck,
    ) -> Result<(Self, Self::Extension), LoadStateError>;
    /// The hash of the state the payload and `extension` hold
    /// ([`state_hash_of`]). Written by
    /// [`savestate_codec!`](crate::savestate_codec).
    fn state_hash(&self, extension: &Self::Extension) -> u64;
    /// The hash of `extension` as a part of the state ([`extension_hash`]),
    /// `None` for the default, which a state does not hold. Written by
    /// [`savestate_codec!`](crate::savestate_codec).
    fn extension_hash(extension: &Self::Extension) -> Option<u64>;
}

/// Writes a payload's [`SavestatePayload::seal`], [`SavestatePayload::open`],
/// [`SavestatePayload::state_hash`] and [`SavestatePayload::extension_hash`]
/// inside its `impl`, as calls to [`seal`], [`open`], [`state_hash_of`] and
/// [`extension_hash`].
///
/// They are written there, in the console's crate, so the layout's reader
/// and writer, and the extension's, are compiled once. A crate compiles its own copy of each
/// generic function it calls (at release optimization no crate reuses
/// another's), so trait methods that called these functions themselves
/// were compiled again in every crate that saves or loads a state: each
/// binding carried a second reader and writer of the whole layout, beside
/// the ones the console's chips use for their own states.
#[macro_export]
macro_rules! savestate_codec {
    () => {
        fn seal(
            &self,
            extension: &<Self as $crate::savestate::SavestatePayload>::Extension,
            rom_sha256: &str,
            created_at: u64,
        ) -> ::core::result::Result<::std::vec::Vec<u8>, ::std::string::String> {
            $crate::savestate::seal(self, extension, rom_sha256, created_at)
        }

        fn open(
            bytes: &[u8],
            rom: ::core::option::Option<$crate::savestate::RomIdentity<'_>>,
            check: $crate::savestate::RomCheck,
        ) -> ::core::result::Result<
            (
                Self,
                <Self as $crate::savestate::SavestatePayload>::Extension,
            ),
            $crate::savestate::LoadStateError,
        > {
            $crate::savestate::open::<Self>(bytes, rom, check)
        }

        fn state_hash(
            &self,
            extension: &<Self as $crate::savestate::SavestatePayload>::Extension,
        ) -> u64 {
            $crate::savestate::state_hash_of(self, extension)
        }

        fn extension_hash(
            extension: &<Self as $crate::savestate::SavestatePayload>::Extension,
        ) -> ::core::option::Option<u64> {
            $crate::savestate::extension_hash::<Self>(extension)
        }
    };
}

/// What a savestate holds after its payload: state the payload's layout
/// has no field for, written only when it is not the default. A build
/// reads its payload and stops, so a build older than an extension loads
/// the state as if the extension were not there. Fields are only ever
/// appended, and `VERSION` counts the sets appended; a build reads a newer
/// set as far as it knows it, and an older set through
/// [`SavestateExtension::read_older`]. The state hash reads it as
/// [`SavestateExtension::hashed`] gives it.
pub trait SavestateExtension: Serialize + DeserializeOwned + Default + PartialEq {
    /// The sets of fields this build knows; 0 for a console with none.
    const VERSION: u32;

    /// The extension an older build wrote at `version`, below
    /// [`Self::VERSION`]. `bytes` are its fields, what follows the
    /// version, read with [`read_extension_part`] as that build wrote
    /// them; the sets appended since take what the machine holds without
    /// them. bincode reads every field in order, so an older set, which
    /// ends before the sets appended since, cannot be read as this
    /// build's. The default refuses; a console overrides it once it
    /// appends a set.
    fn read_older(version: u32, bytes: &[u8]) -> Result<Self, String> {
        let _ = bytes;
        Err(format!(
            "savestate extension version {version} is not read by this build"
        ))
    }

    /// The extension as the state hash reads it ([`state_hash_of`],
    /// [`extension_hash`]), when that is not the extension itself: without
    /// what it carries for the host's sake alone, such as the sound a frame
    /// made so far, which a load plays on and the host's own settings
    /// shape. The savestate holds the whole extension. `None`, the
    /// default, reads the extension as it is.
    fn hashed(&self) -> Option<Self> {
        None
    }
}

/// Fields of an extension an older build wrote, read with the options
/// [`open`] reads an extension with: how a console's
/// [`SavestateExtension::read_older`] reads an older set.
pub fn read_extension_part<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    use bincode::Options;
    bincode_options()
        .deserialize(bytes)
        .map_err(|e| format!("savestate extension unreadable: {e}"))
}

impl SavestateExtension for () {
    const VERSION: u32 = 0;
}

/// The envelope's fields before the payload, read alone.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct SavestateHeader {
    pub envelope_version: u32,
    pub core_id: String,
    pub core_version: u32,
    pub rom_hash: String,
    pub created_at: u64,
}

/// The envelope a state is sealed in, borrowing what it carries: bincode
/// writes it byte for byte as it writes a [`SaveStateEnvelope`].
#[derive(Serialize)]
struct EnvelopeRef<'a, P> {
    envelope_version: u32,
    core_id: &'a str,
    core_version: u32,
    rom_hash: &'a str,
    created_at: u64,
    payload: &'a P,
}

/// The bincode options every reader of the envelope uses: those of
/// `bincode::deserialize`, which stop at the payload's end.
fn bincode_options() -> impl bincode::Options {
    use bincode::Options;
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .allow_trailing_bytes()
}

/// The bytes after the payload: nothing for the default extension, else
/// its version and its fields.
fn extension_bytes<X: SavestateExtension>(extension: &X) -> Result<Vec<u8>, String> {
    if *extension == X::default() {
        return Ok(Vec::new());
    }
    bincode::serialize(&(X::VERSION, extension))
        .map_err(|e| format!("savestate serialize failed: {e}"))
}

/// Seal `payload` and `extension` in an envelope naming the format, the
/// ROM and `created_at`: `[RLSS][zstd(envelope, extension)]`. With the
/// default extension the bytes are those [`serialize_compressed`] writes
/// for the same envelope at level 3.
pub fn seal<P: SavestatePayload>(
    payload: &P,
    extension: &P::Extension,
    rom_sha256: &str,
    created_at: u64,
) -> Result<Vec<u8>, String> {
    let envelope = EnvelopeRef {
        envelope_version: RETRO_SAVESTATE_ENVELOPE_VERSION,
        core_id: P::FORMAT.core_id,
        core_version: P::FORMAT.version,
        rom_hash: rom_sha256,
        created_at,
        payload,
    };
    let mut bytes =
        bincode::serialize(&envelope).map_err(|e| format!("savestate serialize failed: {e}"))?;
    bytes.extend(extension_bytes(extension)?);
    let compressed = zstd::encode_all(&bytes[..], 3)
        .map_err(|e| format!("savestate zstd encode failed: {e}"))?;
    let mut out = Vec::with_capacity(RETRO_SAVESTATE_MAGIC.len() + compressed.len());
    out.extend_from_slice(RETRO_SAVESTATE_MAGIC);
    out.extend_from_slice(&compressed);
    Ok(out)
}

/// The decompressed bytes of an envelope, or the refusal.
fn decompress(bytes: &[u8]) -> Result<Vec<u8>, LoadStateError> {
    zstd::decode_all(&bytes[RETRO_SAVESTATE_MAGIC.len()..])
        .map_err(|e| LoadStateError::Unreadable(format!("savestate zstd decode failed: {e}")))
}

/// The envelope's header, read from its decompressed bytes.
fn header_of(decompressed: &[u8]) -> Result<SavestateHeader, LoadStateError> {
    use bincode::Options;
    bincode_options()
        .deserialize(decompressed)
        .map_err(|e| LoadStateError::Unreadable(format!("savestate deserialize failed: {e}")))
}

/// The header of a sealed state, without its payload.
pub fn read_header(bytes: &[u8]) -> Result<SavestateHeader, LoadStateError> {
    if !has_magic(bytes) {
        return Err(LoadStateError::Unreadable(
            "not a retro-core savestate (missing magic)".to_string(),
        ));
    }
    header_of(&decompress(bytes)?)
}

/// Open a state sealed by [`seal`] (or a form before the envelope, through
/// [`SavestatePayload::open_without_envelope`]), in this order: the
/// envelope's own version, the core id, the layout's version, the payload,
/// the ROM (only with one loaded, `rom`), the payload's own checks, then
/// the extension (the default when none follows the payload).
pub fn open<P: SavestatePayload>(
    bytes: &[u8],
    rom: Option<RomIdentity<'_>>,
    check: RomCheck,
) -> Result<(P, P::Extension), LoadStateError> {
    use bincode::Options;
    if !has_magic(bytes) {
        let payload = P::open_without_envelope(bytes, rom, check)?;
        return Ok((payload, P::Extension::default()));
    }
    let decompressed = decompress(bytes)?;
    let header = header_of(&decompressed)?;
    if header.envelope_version > RETRO_SAVESTATE_ENVELOPE_VERSION {
        return Err(LoadStateError::Unreadable(format!(
            "savestate envelope version {} is newer than supported version {}",
            header.envelope_version, RETRO_SAVESTATE_ENVELOPE_VERSION
        )));
    }
    if header.core_id != P::FORMAT.core_id {
        return Err(LoadStateError::Unreadable(format!(
            "savestate is for core '{}', not '{}'",
            header.core_id,
            P::FORMAT.core_id
        )));
    }
    if header.core_version > P::FORMAT.version {
        return Err(LoadStateError::Unreadable(format!(
            "{} save state core_version {} is newer than supported version {}",
            P::FORMAT.core_id,
            header.core_version,
            P::FORMAT.version
        )));
    }
    // Read from the slice, as every other reader of a savestate does, so
    // the payload's deserializer is the one the build has already; the
    // extension starts where the envelope's bytes end, which bincode's
    // fixed-size integers make its serialized size.
    let envelope: SaveStateEnvelope<P> = bincode_options()
        .deserialize(&decompressed)
        .map_err(|e| LoadStateError::Unreadable(format!("savestate deserialize failed: {e}")))?;
    if let Some(rom) = rom {
        check_rom_hash(&envelope, rom.sha256, check)?;
    }
    envelope.payload.validate()?;
    let extension = if P::Extension::VERSION == 0 {
        P::Extension::default()
    } else {
        let end = bincode_options()
            .serialized_size(&envelope)
            .map_err(|e| LoadStateError::Unreadable(format!("savestate deserialize failed: {e}")))?
            as usize;
        let rest = decompressed.get(end..).unwrap_or_default();
        if rest.is_empty() {
            P::Extension::default()
        } else {
            // The version first: a set this build knows, or a newer one it
            // reads as far as it knows it, is read whole; an older one is
            // the console's to read.
            let version: u32 = bincode_options().deserialize(rest).map_err(|e| {
                LoadStateError::Unreadable(format!("savestate extension unreadable: {e}"))
            })?;
            if version >= P::Extension::VERSION {
                let (_version, extension): (u32, P::Extension) =
                    bincode_options().deserialize(rest).map_err(|e| {
                        LoadStateError::Unreadable(format!("savestate extension unreadable: {e}"))
                    })?;
                extension
            } else {
                let fields = &rest[std::mem::size_of::<u32>()..];
                P::Extension::read_older(version, fields).map_err(LoadStateError::Unreadable)?
            }
        }
    };
    Ok((envelope.payload, extension))
}

/// FNV-1a-64 of the state a savestate holds: the payload's bincode, then
/// the extension's when it is not the default.
pub fn state_hash_of<P: SavestatePayload>(payload: &P, extension: &P::Extension) -> u64 {
    let mut bytes = bincode::serialize(payload).unwrap_or_default();
    let hashed = extension.hashed();
    bytes.extend(extension_bytes(hashed.as_ref().unwrap_or(extension)).unwrap_or_default());
    crate::fnv1a64(&bytes)
}

/// The hash a part of a payload is named by in [`SavestatePayload::hash_parts`].
pub fn part_hash<T: Serialize + ?Sized>(part: &T) -> u64 {
    crate::fnv1a64(&bincode::serialize(part).unwrap_or_default())
}

/// The hash of an extension as a part of the state ([`part_hash`]), `None`
/// for the default, which a state does not hold.
pub fn extension_hash<P: SavestatePayload>(extension: &P::Extension) -> Option<u64> {
    let hashed = extension.hashed();
    let extension = hashed.as_ref().unwrap_or(extension);
    (*extension != P::Extension::default()).then(|| part_hash(extension))
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
    fn check_rom_hash_matches_and_an_empty_hash_is_a_wildcard() {
        let env = envelope(dummy(1));
        check_rom_hash(&env, "abc123", RomCheck::SameRom).unwrap();
        let mut env = envelope(dummy(1));
        env.rom_hash = String::new();
        check_rom_hash(&env, "anything", RomCheck::SameRom).unwrap();
        check_rom_hash(&env, "", RomCheck::SameRom).unwrap();
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
        // The text hosts have always shown.
        assert_eq!(
            err.to_string(),
            "ROM hash mismatch: savestate is for 'abc123' but loaded ROM is 'deadbeef'"
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

    // ---- seal and open

    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
    struct TestPayload {
        version: u32,
        ram: Vec<u8>,
        title: String,
    }

    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
    struct TestExtension {
        busy: u32,
        flag: Option<u8>,
    }

    impl SavestateExtension for TestExtension {
        const VERSION: u32 = 1;
    }

    impl SavestatePayload for TestPayload {
        crate::savestate_codec!();

        const FORMAT: SavestateFormat = SavestateFormat {
            core_id: "test",
            version: 2,
        };
        type Extension = TestExtension;
        fn validate(&self) -> Result<(), String> {
            if self.version > 2 {
                return Err(format!("test payload version {} is newer", self.version));
            }
            Ok(())
        }
        fn hash_parts(&self) -> Vec<(&'static str, u64)> {
            vec![
                ("ram", part_hash(&self.ram)),
                ("title", part_hash(&self.title)),
            ]
        }
    }

    /// A payload with no extension, sealed as the envelope always was.
    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
    struct PlainPayload {
        cycles: u64,
        regs: [u16; 8],
    }

    impl SavestatePayload for PlainPayload {
        crate::savestate_codec!();

        const FORMAT: SavestateFormat = SavestateFormat {
            core_id: "plain",
            version: 4,
        };
        type Extension = ();
        fn validate(&self) -> Result<(), String> {
            Ok(())
        }
        fn hash_parts(&self) -> Vec<(&'static str, u64)> {
            vec![("cycles", self.cycles)]
        }
    }

    fn test_payload() -> TestPayload {
        TestPayload {
            version: 2,
            ram: (0..512u32).map(|i| (i * 7) as u8).collect(),
            title: "TEST".to_string(),
        }
    }

    const ROM: RomIdentity<'static> = RomIdentity {
        sha256: "aa11",
        title: "TEST",
    };

    #[test]
    fn seal_with_no_extension_writes_the_envelope_byte_for_byte() {
        let payload = test_payload();
        let sealed = seal(&payload, &TestExtension::default(), "aa11", 77).unwrap();
        let old = serialize_compressed(
            &SaveStateEnvelope {
                envelope_version: RETRO_SAVESTATE_ENVELOPE_VERSION,
                core_id: "test".to_string(),
                core_version: 2,
                rom_hash: "aa11".to_string(),
                created_at: 77,
                payload: payload.clone(),
            },
            3,
        )
        .unwrap();
        assert_eq!(sealed, old);

        let plain = PlainPayload {
            cycles: 12345,
            regs: [1, 2, 3, 4, 5, 6, 7, 8],
        };
        let sealed = seal(&plain, &(), "", 0).unwrap();
        let old = serialize_compressed(
            &SaveStateEnvelope {
                envelope_version: RETRO_SAVESTATE_ENVELOPE_VERSION,
                core_id: "plain".to_string(),
                core_version: 4,
                rom_hash: String::new(),
                created_at: 0,
                payload: plain.clone(),
            },
            3,
        )
        .unwrap();
        assert_eq!(sealed, old);
    }

    #[test]
    fn an_extension_is_written_only_when_it_holds_something_and_read_back() {
        let payload = test_payload();
        let none = seal(&payload, &TestExtension::default(), "aa11", 0).unwrap();
        let ext = TestExtension {
            busy: 9,
            flag: Some(3),
        };
        let some = seal(&payload, &ext, "aa11", 0).unwrap();
        assert_ne!(none, some);
        let (p, x) = open::<TestPayload>(&some, Some(ROM), RomCheck::SameRom).unwrap();
        assert_eq!((p, x), (payload.clone(), ext));
        let (_, x) = open::<TestPayload>(&none, Some(ROM), RomCheck::SameRom).unwrap();
        assert_eq!(x, TestExtension::default());
    }

    #[test]
    fn a_reader_of_the_envelope_alone_reads_past_the_extension() {
        let payload = test_payload();
        let ext = TestExtension {
            busy: 1,
            flag: None,
        };
        let sealed = seal(&payload, &ext, "aa11", 5).unwrap();
        // What every build before the extension does.
        let env = deserialize_compressed::<TestPayload>(&sealed).unwrap();
        assert_eq!(env.payload, payload);
        assert_eq!(env.created_at, 5);
    }

    /// The test payload sealed with `extension` after it as it stands: a
    /// version and fields another build wrote.
    fn sealed_with_extension_bytes(extension: &[u8]) -> Vec<u8> {
        let mut bytes = bincode::serialize(&SaveStateEnvelope {
            envelope_version: RETRO_SAVESTATE_ENVELOPE_VERSION,
            core_id: "test".to_string(),
            core_version: 2,
            rom_hash: "aa11".to_string(),
            created_at: 0,
            payload: test_payload(),
        })
        .unwrap();
        bytes.extend_from_slice(extension);
        let mut sealed = RETRO_SAVESTATE_MAGIC.to_vec();
        sealed.extend(zstd::encode_all(&bytes[..], 3).unwrap());
        sealed
    }

    #[test]
    fn a_newer_extension_is_read_as_far_as_this_build_knows_it() {
        #[derive(Serialize)]
        struct Newer {
            busy: u32,
            flag: Option<u8>,
            later: u64,
        }
        let sealed = sealed_with_extension_bytes(
            &bincode::serialize(&(
                2u32,
                Newer {
                    busy: 4,
                    flag: Some(1),
                    later: 99,
                },
            ))
            .unwrap(),
        );
        let (_, x) = open::<TestPayload>(&sealed, Some(ROM), RomCheck::SameRom).unwrap();
        assert_eq!(
            x,
            TestExtension {
                busy: 4,
                flag: Some(1)
            }
        );
    }

    /// The test console in a later build, whose extension appended a set:
    /// `later`, which the machine holds at 0 without it.
    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
    struct LaterPayload {
        version: u32,
        ram: Vec<u8>,
        title: String,
    }

    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
    struct LaterExtension {
        busy: u32,
        flag: Option<u8>,
        later: u64,
    }

    impl SavestateExtension for LaterExtension {
        const VERSION: u32 = 2;

        fn read_older(version: u32, bytes: &[u8]) -> Result<Self, String> {
            match version {
                1 => {
                    let (busy, flag): (u32, Option<u8>) = read_extension_part(bytes)?;
                    Ok(Self {
                        busy,
                        flag,
                        later: 0,
                    })
                }
                _ => Err(format!("no version {version}")),
            }
        }
    }

    impl SavestatePayload for LaterPayload {
        crate::savestate_codec!();

        const FORMAT: SavestateFormat = SavestateFormat {
            core_id: "test",
            version: 2,
        };
        type Extension = LaterExtension;
        fn validate(&self) -> Result<(), String> {
            Ok(())
        }
        fn hash_parts(&self) -> Vec<(&'static str, u64)> {
            Vec::new()
        }
    }

    #[test]
    fn an_older_extension_is_read_by_the_console_from_its_own_fields() {
        // The earlier build seals its set.
        let older = seal(
            &test_payload(),
            &TestExtension {
                busy: 4,
                flag: Some(1),
            },
            "aa11",
            0,
        )
        .unwrap();
        let (payload, x) = open::<LaterPayload>(&older, Some(ROM), RomCheck::SameRom).unwrap();
        assert_eq!(payload.ram, test_payload().ram);
        assert_eq!(
            x,
            LaterExtension {
                busy: 4,
                flag: Some(1),
                later: 0
            }
        );
        // The later build's own set reads whole, and the earlier build
        // reads it as far as it knows it.
        let later = LaterExtension {
            busy: 5,
            flag: None,
            later: 77,
        };
        let sealed = seal(&payload, &later, "aa11", 0).unwrap();
        let (_, x) = open::<LaterPayload>(&sealed, Some(ROM), RomCheck::SameRom).unwrap();
        assert_eq!(x, later);
        let (_, x) = open::<TestPayload>(&sealed, Some(ROM), RomCheck::SameRom).unwrap();
        assert_eq!(
            x,
            TestExtension {
                busy: 5,
                flag: None
            }
        );
    }

    #[test]
    fn an_older_extension_no_reader_knows_is_refused_with_its_text() {
        let sealed = sealed_with_extension_bytes(&bincode::serialize(&(0u32, 9u32)).unwrap());
        assert_eq!(
            refusal(&sealed, Some(ROM), RomCheck::SameRom),
            "savestate extension version 0 is not read by this build"
        );
        // A console's reader refuses in its own words, and a set cut short
        // is unreadable.
        let older = sealed_with_extension_bytes(&bincode::serialize(&(0u32, 1u8)).unwrap());
        assert_eq!(
            open::<LaterPayload>(&older, Some(ROM), RomCheck::SameRom).unwrap_err(),
            LoadStateError::Unreadable("no version 0".into())
        );
        let short = sealed_with_extension_bytes(&bincode::serialize(&(1u32, 9u16)).unwrap());
        let text = open::<LaterPayload>(&short, Some(ROM), RomCheck::SameRom)
            .unwrap_err()
            .to_string();
        assert!(
            text.starts_with("savestate extension unreadable: "),
            "{text}"
        );
    }

    fn reseal(change: impl Fn(&mut SaveStateEnvelope<TestPayload>)) -> Vec<u8> {
        let mut env = SaveStateEnvelope {
            envelope_version: RETRO_SAVESTATE_ENVELOPE_VERSION,
            core_id: "test".to_string(),
            core_version: 2,
            rom_hash: "aa11".to_string(),
            created_at: 0,
            payload: test_payload(),
        };
        change(&mut env);
        serialize_compressed(&env, 3).unwrap()
    }

    fn refusal(bytes: &[u8], rom: Option<RomIdentity<'_>>, check: RomCheck) -> String {
        open::<TestPayload>(bytes, rom, check)
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn open_refuses_in_its_order_with_its_texts() {
        let rom = Some(ROM);
        assert_eq!(
            refusal(b"garbage", rom, RomCheck::SameRom),
            "not a retro-core savestate (missing magic)"
        );
        assert_eq!(
            refusal(&[], rom, RomCheck::SameRom),
            "not a retro-core savestate (missing magic)"
        );
        assert!(refusal(b"RLSSnot zstd", rom, RomCheck::SameRom)
            .starts_with("savestate zstd decode failed: "));
        assert_eq!(
            refusal(&reseal(|e| e.envelope_version = 2), rom, RomCheck::SameRom),
            "savestate envelope version 2 is newer than supported version 1"
        );
        assert_eq!(
            refusal(
                &reseal(|e| e.core_id = "other".into()),
                rom,
                RomCheck::SameRom
            ),
            "savestate is for core 'other', not 'test'"
        );
        assert_eq!(
            refusal(&reseal(|e| e.core_version = 3), rom, RomCheck::SameRom),
            "test save state core_version 3 is newer than supported version 2"
        );
        // The version is refused before the ROM.
        assert_eq!(
            refusal(
                &reseal(|e| {
                    e.core_version = 3;
                    e.rom_hash = "ff".into();
                }),
                rom,
                RomCheck::SameRom
            ),
            "test save state core_version 3 is newer than supported version 2"
        );
        let other = reseal(|e| e.rom_hash = "ff".into());
        assert_eq!(
            open::<TestPayload>(&other, rom, RomCheck::SameRom).unwrap_err(),
            LoadStateError::OtherRom {
                state_rom: "ff".into(),
                loaded_rom: "aa11".into()
            }
        );
        assert!(open::<TestPayload>(&other, rom, RomCheck::AnyRom).is_ok());
        // No ROM loaded: no ROM to refuse the state for.
        assert!(open::<TestPayload>(&other, None, RomCheck::SameRom).is_ok());
        assert_eq!(
            refusal(&reseal(|e| e.payload.version = 9), rom, RomCheck::SameRom),
            "test payload version 9 is newer"
        );
        // An empty hash is a wildcard.
        assert!(
            open::<TestPayload>(&reseal(|e| e.rom_hash.clear()), rom, RomCheck::SameRom).is_ok()
        );
    }

    #[test]
    fn the_header_reads_alone() {
        let sealed = seal(&test_payload(), &TestExtension::default(), "aa11", 42).unwrap();
        assert_eq!(
            read_header(&sealed).unwrap(),
            SavestateHeader {
                envelope_version: 1,
                core_id: "test".into(),
                core_version: 2,
                rom_hash: "aa11".into(),
                created_at: 42
            }
        );
        assert!(read_header(b"nope").is_err());
    }

    /// An extension that carries a part for the host alone.
    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
    struct HostPartExtension {
        busy: u32,
        heard: Vec<u8>,
    }

    impl SavestateExtension for HostPartExtension {
        const VERSION: u32 = 1;

        fn hashed(&self) -> Option<Self> {
            (!self.heard.is_empty()).then(|| Self {
                busy: self.busy,
                heard: Vec::new(),
            })
        }
    }

    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
    struct HostPartPayload {
        cycles: u64,
    }

    impl SavestatePayload for HostPartPayload {
        crate::savestate_codec!();

        const FORMAT: SavestateFormat = SavestateFormat {
            core_id: "host-part",
            version: 1,
        };
        type Extension = HostPartExtension;
        fn validate(&self) -> Result<(), String> {
            Ok(())
        }
        fn hash_parts(&self) -> Vec<(&'static str, u64)> {
            vec![("cycles", part_hash(&self.cycles))]
        }
    }

    /// The state hash reads an extension as its `hashed` gives it: a part
    /// for the host alone moves no hash, and an extension that holds
    /// nothing else is no part of the state; the savestate holds it whole.
    #[test]
    fn the_state_hash_reads_the_extension_as_hashed() {
        let payload = HostPartPayload { cycles: 7 };
        let extension = |busy, heard: &[u8]| HostPartExtension {
            busy,
            heard: heard.to_vec(),
        };
        let hash = |e: &HostPartExtension| state_hash_of(&payload, e);
        assert_eq!(hash(&extension(1, &[4, 5])), hash(&extension(1, &[])));
        assert_ne!(hash(&extension(1, &[4, 5])), hash(&extension(2, &[4, 5])));
        assert_eq!(
            hash(&extension(0, &[9])),
            hash(&HostPartExtension::default())
        );
        assert_eq!(
            extension_hash::<HostPartPayload>(&extension(0, &[9])),
            None,
            "nothing but the host's part"
        );
        assert_eq!(
            extension_hash::<HostPartPayload>(&extension(3, &[9])),
            extension_hash::<HostPartPayload>(&extension(3, &[]))
        );
        let sealed = payload.seal(&extension(1, &[4, 5]), "", 0).unwrap();
        let (_, opened) = HostPartPayload::open(&sealed, None, RomCheck::AnyRom).unwrap();
        assert_eq!(
            opened,
            extension(1, &[4, 5]),
            "the savestate holds it whole"
        );
    }

    #[test]
    fn the_state_hash_covers_the_payload_then_the_extension() {
        let payload = test_payload();
        let plain = state_hash_of(&payload, &TestExtension::default());
        assert_eq!(
            plain,
            crate::fnv1a64(&bincode::serialize(&payload).unwrap())
        );
        let with = state_hash_of(
            &payload,
            &TestExtension {
                busy: 1,
                flag: None,
            },
        );
        assert_ne!(plain, with);
        assert_eq!(payload.hash_parts()[0].1, part_hash(&payload.ram));
    }
}
