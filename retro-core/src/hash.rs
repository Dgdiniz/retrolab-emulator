//! Small, dependency-free hashing helpers shared across cores.

/// FNV-1a 64-bit offset basis (the empty-input hash).
pub const FNV_OFFSET_BASIS_64: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
pub const FNV_PRIME_64: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a 64-bit hash of a byte slice.
///
/// A fast, non-cryptographic hash used to fingerprint a canonicalized
/// machine-state buffer once per frame. The only property callers rely
/// on is determinism: identical bytes always yield the same `u64` on
/// every platform, so two peers (or the determinism harness's two
/// instances) that are in the same state hash to the same value. It is
/// a divergence/corruption detector, NOT collision-resistant against an
/// adversary, do not use it where a cryptographic hash is required.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET_BASIS_64;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME_64);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_is_offset_basis() {
        assert_eq!(fnv1a64(b""), FNV_OFFSET_BASIS_64);
    }

    #[test]
    fn matches_known_fnv1a64_vectors() {
        // Canonical FNV-1a 64-bit reference vectors.
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn distinct_inputs_hash_differently() {
        assert_ne!(fnv1a64(b"hello"), fnv1a64(b"world"));
    }

    #[test]
    fn single_bit_change_changes_hash() {
        assert_ne!(fnv1a64(&[0x00]), fnv1a64(&[0x01]));
    }
}
