//! DSP-1 coprocessor firmware loading.
//!
//! The DSP-1 (and its 1A/1B revisions) is an NEC uPD7725 with
//! mask-programmed internal ROM. The dump is a fixed **8192-byte** blob:
//! 2048 24-bit program words followed by 1024 16-bit data-ROM words. Two
//! byte orders circulate in the wild:
//!
//! - `.bin`: **big-endian** words: the file bsnes-plus and higan ask
//!   for (`dsp1b.bin`).
//! - `.rom`: **little-endian** words: the form snes9x and later bsnes
//!   builds prefer.
//!
//! Both orders decode to the *identical* canonical firmware (verified:
//! the first program word is `0x97C000` = `JRQM 0` either way), so the
//! emulated DSP behaves the same regardless of which file the user
//! supplies, only the on-disk byte order differs.
//!
//! [`Dsp1Firmware::from_bytes`] decodes a blob under an explicit
//! [`ByteOrder`], or auto-detects the order *and* the revision by
//! SHA-256 against the four known dumps (DSP-1/DSP-1B x BE/LE). On an
//! unrecognised digest, `Auto` falls back to little-endian (`.rom`,
//! the modern canonical form) and reports `revision: None` so the
//! caller can warn.

use sha2::{Digest, Sha256};

/// Total DSP-1 firmware size in bytes (2048x3 program + 1024x2 data).
pub const FIRMWARE_SIZE: usize = 8192;
/// Program words (24-bit each).
const PROGRAM_WORDS: usize = 2048;
/// Data-ROM words (16-bit each).
const DATA_ROM_WORDS: usize = 1024;
/// Byte offset where the data ROM begins (after the program region).
const DATA_ROM_BASE: usize = PROGRAM_WORDS * 3; // 6144

/// SHA-256 of `dsp1.bin` (DSP-1, big-endian).
pub const DSP1_SHA256_BE: &str = "5f2e5ed06b362be023b978b5978813ecb9a07c76592454b45c2a1ed17a0de349";
/// SHA-256 of `dsp1.rom` (DSP-1, little-endian).
pub const DSP1_SHA256_LE: &str = "91e87d11e1c30d172556bed2211cce2efa94ba595f58c5d264809ef4d363a97b";
/// SHA-256 of `dsp1b.bin` (DSP-1B, big-endian).
pub const DSP1B_SHA256_BE: &str =
    "4d42db0f36faef263d6b93f508e8c1c4ae8fc2605fd35e3390ecc02905cd420c";
/// SHA-256 of `dsp1b.rom` (DSP-1B, little-endian).
pub const DSP1B_SHA256_LE: &str =
    "d789cb3c36b05c0b23b6c6f23be7aa37c6e78b6ee9ceac8d2d2aa9d8c4d35fa9";

/// How firmware words are laid out on disk.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ByteOrder {
    /// Detect order (and revision) by SHA-256; fall back to little-endian.
    Auto,
    /// `.bin` form, big-endian words.
    BigEndian,
    /// `.rom` form, little-endian words.
    LittleEndian,
}

/// Recognised DSP-1 silicon revision.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Dsp1Revision {
    /// Original DSP-1 / DSP-1A.
    Dsp1,
    /// DSP-1B (bug-fixed revision; what Super Mario Kart shipped with).
    Dsp1B,
}

impl Dsp1Revision {
    /// Human-readable name for diagnostics / CLI output.
    pub fn name(self) -> &'static str {
        match self {
            Dsp1Revision::Dsp1 => "DSP-1",
            Dsp1Revision::Dsp1B => "DSP-1B",
        }
    }
}

/// Errors decoding a firmware blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirmwareError {
    /// The blob was not exactly [`FIRMWARE_SIZE`] bytes.
    WrongSize { expected: usize, actual: usize },
}

impl std::fmt::Display for FirmwareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FirmwareError::WrongSize { expected, actual } => {
                write!(f, "DSP-1 firmware must be {expected} bytes, got {actual}")
            }
        }
    }
}

impl std::error::Error for FirmwareError {}

/// Decoded DSP-1 firmware, ready for the NEC uPD7725 core (the `upd7725`
/// crate, built with the `nec-lle` feature). The decoder itself needs no
/// core: it only splits and byte-orders the dump.
#[derive(Clone, Debug)]
pub struct Dsp1Firmware {
    /// 2048 24-bit program words.
    pub program: Vec<u32>,
    /// 1024 16-bit data-ROM words.
    pub data_rom: Vec<u16>,
    /// Revision identified by SHA-256, or `None` when the digest is
    /// unrecognised (the bytes still decode under the resolved order).
    pub revision: Option<Dsp1Revision>,
    /// The byte order actually used to decode (never [`ByteOrder::Auto`]).
    pub order: ByteOrder,
}

impl Dsp1Firmware {
    /// Decode `bytes` into program + data ROM.
    ///
    /// With [`ByteOrder::Auto`], the order and revision are detected by
    /// SHA-256; an unknown digest falls back to little-endian with
    /// `revision: None`. With an explicit order, the bytes are decoded
    /// as requested and the revision is only set when the digest matches
    /// a known dump *in that order*.
    pub fn from_bytes(bytes: &[u8], requested: ByteOrder) -> Result<Self, FirmwareError> {
        if bytes.len() != FIRMWARE_SIZE {
            return Err(FirmwareError::WrongSize {
                expected: FIRMWARE_SIZE,
                actual: bytes.len(),
            });
        }

        let detected = detect(bytes);
        let (revision, order) = match requested {
            ByteOrder::Auto => match detected {
                Some((rev, ord)) => (Some(rev), ord),
                None => (None, ByteOrder::LittleEndian),
            },
            explicit => {
                let rev = detected.and_then(|(r, o)| (o == explicit).then_some(r));
                (rev, explicit)
            }
        };

        let (program, data_rom) = decode(bytes, order);
        Ok(Self {
            program,
            data_rom,
            revision,
            order,
        })
    }

    /// `true` when the firmware's SHA-256 matched a known DSP-1 dump.
    pub fn is_recognised(&self) -> bool {
        self.revision.is_some()
    }
}

/// SHA-256 over the raw firmware bytes, hex-encoded lowercase.
fn firmware_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Match a blob's digest against the four known DSP-1 dumps.
fn detect(bytes: &[u8]) -> Option<(Dsp1Revision, ByteOrder)> {
    let digest = firmware_sha256(bytes);
    match digest.as_str() {
        DSP1_SHA256_BE => Some((Dsp1Revision::Dsp1, ByteOrder::BigEndian)),
        DSP1_SHA256_LE => Some((Dsp1Revision::Dsp1, ByteOrder::LittleEndian)),
        DSP1B_SHA256_BE => Some((Dsp1Revision::Dsp1B, ByteOrder::BigEndian)),
        DSP1B_SHA256_LE => Some((Dsp1Revision::Dsp1B, ByteOrder::LittleEndian)),
        _ => None,
    }
}

/// Decode the 8192-byte blob into program + data ROM under `order`.
/// `order` must be `BigEndian` or `LittleEndian` (the caller resolves
/// `Auto` first); a stray `Auto` is treated as little-endian.
fn decode(bytes: &[u8], order: ByteOrder) -> (Vec<u32>, Vec<u16>) {
    let big_endian = matches!(order, ByteOrder::BigEndian);

    let mut program = Vec::with_capacity(PROGRAM_WORDS);
    for i in 0..PROGRAM_WORDS {
        let b = &bytes[i * 3..i * 3 + 3];
        let word = if big_endian {
            ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32)
        } else {
            ((b[2] as u32) << 16) | ((b[1] as u32) << 8) | (b[0] as u32)
        };
        program.push(word);
    }

    let mut data_rom = Vec::with_capacity(DATA_ROM_WORDS);
    for i in 0..DATA_ROM_WORDS {
        let b = &bytes[DATA_ROM_BASE + i * 2..DATA_ROM_BASE + i * 2 + 2];
        let word = if big_endian {
            ((b[0] as u16) << 8) | (b[1] as u16)
        } else {
            ((b[1] as u16) << 8) | (b[0] as u16)
        };
        data_rom.push(word);
    }

    (program, data_rom)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Re-encode canonical words into an 8192-byte blob under `order`
    /// (inverse of [`decode`]) so tests can round-trip without real
    /// firmware.
    fn encode(program: &[u32], data_rom: &[u16], big_endian: bool) -> Vec<u8> {
        let mut out = vec![0u8; FIRMWARE_SIZE];
        for (i, &w) in program.iter().enumerate() {
            let b = &mut out[i * 3..i * 3 + 3];
            if big_endian {
                b[0] = (w >> 16) as u8;
                b[1] = (w >> 8) as u8;
                b[2] = w as u8;
            } else {
                b[2] = (w >> 16) as u8;
                b[1] = (w >> 8) as u8;
                b[0] = w as u8;
            }
        }
        for (i, &w) in data_rom.iter().enumerate() {
            let b = &mut out[DATA_ROM_BASE + i * 2..DATA_ROM_BASE + i * 2 + 2];
            if big_endian {
                b[0] = (w >> 8) as u8;
                b[1] = w as u8;
            } else {
                b[1] = (w >> 8) as u8;
                b[0] = w as u8;
            }
        }
        out
    }

    fn synthetic_words() -> (Vec<u32>, Vec<u16>) {
        let program: Vec<u32> = (0..PROGRAM_WORDS)
            .map(|i| ((i as u32).wrapping_mul(0x111) ^ 0x5A) & 0x00FF_FFFF)
            .collect();
        let data_rom: Vec<u16> = (0..DATA_ROM_WORDS)
            .map(|i| (i as u16).wrapping_mul(7).wrapping_add(0x1234))
            .collect();
        (program, data_rom)
    }

    #[test]
    fn rejects_wrong_size() {
        let err = Dsp1Firmware::from_bytes(&[0u8; 100], ByteOrder::Auto).unwrap_err();
        assert_eq!(
            err,
            FirmwareError::WrongSize {
                expected: 8192,
                actual: 100
            }
        );
    }

    #[test]
    fn big_endian_decode_matches_layout() {
        let (program, data_rom) = synthetic_words();
        let blob = encode(&program, &data_rom, true);
        let fw = Dsp1Firmware::from_bytes(&blob, ByteOrder::BigEndian).unwrap();
        assert_eq!(fw.program, program);
        assert_eq!(fw.data_rom, data_rom);
        assert_eq!(fw.order, ByteOrder::BigEndian);
    }

    #[test]
    fn both_orders_decode_to_same_canonical_firmware() {
        // The load-bearing property behind dual-format support: a `.bin`
        // (BE layout) and a `.rom` (LE layout) of the same firmware
        // decode to identical program + data words.
        let (program, data_rom) = synthetic_words();
        let be_blob = encode(&program, &data_rom, true);
        let le_blob = encode(&program, &data_rom, false);
        assert_ne!(be_blob, le_blob, "the two on-disk layouts differ");

        let be = Dsp1Firmware::from_bytes(&be_blob, ByteOrder::BigEndian).unwrap();
        let le = Dsp1Firmware::from_bytes(&le_blob, ByteOrder::LittleEndian).unwrap();
        assert_eq!(be.program, le.program);
        assert_eq!(be.data_rom, le.data_rom);
        assert_eq!(be.program, program);
    }

    #[test]
    fn auto_falls_back_to_little_endian_when_unrecognised() {
        let (program, data_rom) = synthetic_words();
        let blob = encode(&program, &data_rom, false);
        let fw = Dsp1Firmware::from_bytes(&blob, ByteOrder::Auto).unwrap();
        assert!(!fw.is_recognised());
        assert_eq!(fw.revision, None);
        assert_eq!(fw.order, ByteOrder::LittleEndian);
        // Decoded as LE, so it round-trips against the LE-encoded blob.
        assert_eq!(fw.program, program);
    }

    #[test]
    fn revision_name_strings() {
        assert_eq!(Dsp1Revision::Dsp1.name(), "DSP-1");
        assert_eq!(Dsp1Revision::Dsp1B.name(), "DSP-1B");
    }
}
