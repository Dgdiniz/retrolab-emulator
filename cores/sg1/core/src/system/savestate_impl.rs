/**
 * Save State Implementation
 *
 * Compression and serialization helpers
 */
use super::savestate::{SystemSaveState, SAVE_STATE_VERSION};

/// Compress data using zstd
pub fn compress_zstd(data: &[u8], level: i32) -> Result<Vec<u8>, String> {
    zstd::encode_all(data, level).map_err(|e| format!("Compression failed: {}", e))
}

/// Decompress data using zstd
pub fn decompress_zstd(data: &[u8]) -> Result<Vec<u8>, String> {
    zstd::decode_all(data).map_err(|e| format!("Decompression failed: {}", e))
}

/// Serialize and compress a save state
pub fn serialize_and_compress(
    state: &SystemSaveState,
    compression_level: i32,
) -> Result<Vec<u8>, String> {
    // Serialize to bincode
    let serialized =
        bincode::serialize(state).map_err(|e| format!("Serialization failed: {}", e))?;

    // Compress
    compress_zstd(&serialized, compression_level)
}

/// Decompress and deserialize a save state
pub fn decompress_and_deserialize(data: &[u8]) -> Result<SystemSaveState, String> {
    // Decompress
    let decompressed = decompress_zstd(data)?;

    // Deserialize
    let state: SystemSaveState = bincode::deserialize(&decompressed)
        .map_err(|e| format!("Deserialization failed: {}", e))?;

    // Validate
    state.validate()?;

    // Check version
    if state.version > SAVE_STATE_VERSION {
        return Err(format!(
            "Save state version {} is newer than supported version {}",
            state.version, SAVE_STATE_VERSION
        ));
    }

    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compression_roundtrip() {
        let data = b"Hello, world! This is a test of zstd compression.";
        let compressed = compress_zstd(data, 3).unwrap();
        let decompressed = decompress_zstd(&compressed).unwrap();
        assert_eq!(data.as_slice(), decompressed.as_slice());
    }

    #[test]
    fn test_compression_reduces_size() {
        let data = vec![0u8; 10000]; // Highly compressible
        let compressed = compress_zstd(&data, 3).unwrap();
        assert!(compressed.len() < data.len());
        println!(
            "Original: {} bytes, Compressed: {} bytes",
            data.len(),
            compressed.len()
        );
    }
}
