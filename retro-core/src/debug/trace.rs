//! Generic streaming trace buffer for CPU instruction-level traces.
//!
//! `TracingState` is the storage half of the tracing pipeline: it owns a
//! pre-allocated byte buffer, accumulates serialized trace entries, and
//! signals when the buffer is ready to be flushed (typically to disk via
//! the host, e.g. OPFS in the browser). The buffer doubles in size when
//! full, up to a configurable hard cap.
//!
//! What lives here vs. what lives in the per-CPU core:
//! - **Here**: buffer management, enable flag, flush signaling, statistics.
//! - **In the core**: the actual binary entry format (struct layout +
//!   `to_bytes()` serialization). Different CPUs use different shapes
//!   (e.g. SG1's 84-byte 68K entry vs. 38-byte Z80 entry, SNES's
//!   65816/SPC700 entries), so the core converts its struct to bytes and
//!   pushes the slice through [`TracingState::push_entry`].
//!
//! Default sizes (initial buffer 8 MiB, hard cap 256 MiB) are the
//! values SG1 and SNES1 use; tune via `new()` if you need
//! something smaller for tests or larger for offline runs.

/// Maximum buffer size before entries start being dropped (256 MiB).
///
/// The buffer doubles on demand from its initial size up to this cap.
/// Past the cap a flush-needed flag is raised and the latest entry is
/// dropped; this is the back-pressure signal so the host can drain the
/// buffer instead of unboundedly growing memory.
pub const MAX_TRACE_BUFFER_SIZE: usize = 256 * 1024 * 1024;

/// Streaming binary trace buffer.
///
/// Cheap to clock (`push_entry` is a `copy_from_slice` + bump); the host
/// owns the flush loop and decides what to do with the bytes (write to
/// file, send over a channel, etc.).
pub struct TracingState {
    enabled: bool,
    buffer: Vec<u8>,
    buffer_pos: usize,
    flush_threshold: usize,
    needs_flush: bool,
    total_entries: u64,
    flushed_entries: u64,
    entry_size: usize,
}

impl TracingState {
    /// Create a tracing buffer with explicit capacity and entry size.
    ///
    /// - `buffer_size_mb`: initial buffer in MiB. The buffer will grow
    ///   up to [`MAX_TRACE_BUFFER_SIZE`] on demand.
    /// - `entry_size`: serialized size of every entry pushed via
    ///   [`Self::push_entry`]. Used to compute entry counts from byte
    ///   counts and to ensure pushes are sized consistently.
    pub fn new(buffer_size_mb: usize, entry_size: usize) -> Self {
        let buffer_size = buffer_size_mb * 1024 * 1024;
        Self {
            enabled: false,
            buffer: vec![0u8; buffer_size],
            buffer_pos: 0,
            flush_threshold: buffer_size,
            needs_flush: false,
            total_entries: 0,
            flushed_entries: 0,
            entry_size,
        }
    }

    /// Enable or disable tracing. Enabling from a disabled state resets
    /// the buffer position and counters so the trace always starts from
    /// a clean slate.
    pub fn set_enabled(&mut self, enabled: bool) {
        if enabled && !self.enabled {
            self.buffer_pos = 0;
            self.total_entries = 0;
            self.flushed_entries = 0;
            self.needs_flush = false;
        }
        self.enabled = enabled;
    }

    /// Whether tracing is currently active.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Append a serialized trace entry. Returns `true` when the buffer
    /// reached its flush threshold (the host should drain via
    /// [`Self::get_flush_data`] + [`Self::clear_flushed`]).
    ///
    /// Pushes when `is_enabled() == false` are no-ops.
    ///
    /// If the buffer is full and already at the hard cap, the entry is
    /// dropped to bound memory; `needs_flush` is set so the next host
    /// poll drains and frees space.
    pub fn push_entry(&mut self, bytes: &[u8]) -> bool {
        if !self.enabled {
            return false;
        }

        if self.buffer_pos + bytes.len() > self.buffer.len() {
            let new_size = (self.buffer.len() * 2).min(MAX_TRACE_BUFFER_SIZE);
            if new_size > self.buffer.len() {
                self.buffer.resize(new_size, 0);
                self.flush_threshold = new_size;
            } else {
                self.needs_flush = true;
                return true;
            }
            self.needs_flush = true;
        }

        self.buffer[self.buffer_pos..self.buffer_pos + bytes.len()].copy_from_slice(bytes);
        self.buffer_pos += bytes.len();
        self.total_entries += 1;

        if self.buffer_pos >= self.flush_threshold {
            self.needs_flush = true;
        }

        self.needs_flush
    }

    /// `true` when the buffer has reached the threshold and the host
    /// should drain it.
    pub fn needs_flush(&self) -> bool {
        self.needs_flush
    }

    /// Slice covering all pending bytes. Stays valid until
    /// [`Self::clear_flushed`] is called.
    pub fn get_flush_data(&self) -> &[u8] {
        &self.buffer[..self.buffer_pos]
    }

    /// Mark the pending bytes as flushed: advance `flushed_entries`,
    /// reset the write cursor, and clear the `needs_flush` signal.
    pub fn clear_flushed(&mut self) {
        let entries_in_buffer = self.buffer_pos / self.entry_size;
        self.flushed_entries += entries_in_buffer as u64;
        self.buffer_pos = 0;
        self.needs_flush = false;
    }

    /// `true` when the buffer holds any pending bytes (even below the
    /// flush threshold). Useful at shutdown / pause to drain leftovers.
    pub fn has_data(&self) -> bool {
        self.buffer_pos > 0
    }

    /// Total entries pushed since tracing was last enabled.
    pub fn total_entries(&self) -> u64 {
        self.total_entries
    }

    /// Entries already drained via [`Self::clear_flushed`].
    pub fn flushed_entries(&self) -> u64 {
        self.flushed_entries
    }

    /// Entries currently buffered but not yet flushed.
    pub fn buffered_entries(&self) -> u64 {
        (self.buffer_pos / self.entry_size) as u64
    }

    /// Bytes currently buffered.
    pub fn buffer_bytes(&self) -> usize {
        self.buffer_pos
    }

    /// Current allocated capacity (may grow up to [`MAX_TRACE_BUFFER_SIZE`]).
    pub fn buffer_capacity(&self) -> usize {
        self.buffer.len()
    }
}

/// Snapshot of [`TracingState`] internals, suitable for serialization
/// (the debugger UI consumes it as JSON).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TraceStats {
    pub total_entries: u64,
    pub flushed_entries: u64,
    pub buffered_entries: u64,
    pub buffer_bytes: usize,
    pub buffer_capacity: usize,
    pub enabled: bool,
}

impl TraceStats {
    /// Build a stats snapshot from a [`TracingState`].
    pub fn from_state(state: &TracingState) -> Self {
        Self {
            total_entries: state.total_entries(),
            flushed_entries: state.flushed_entries(),
            buffered_entries: state.buffered_entries(),
            buffer_bytes: state.buffer_bytes(),
            buffer_capacity: state.buffer_capacity(),
            enabled: state.is_enabled(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENTRY_SIZE: usize = 8;

    fn entry(value: u8) -> [u8; ENTRY_SIZE] {
        [value; ENTRY_SIZE]
    }

    #[test]
    fn new_starts_disabled_and_empty() {
        let state = TracingState::new(1, ENTRY_SIZE);
        assert!(!state.is_enabled());
        assert_eq!(state.total_entries(), 0);
        assert_eq!(state.flushed_entries(), 0);
        assert_eq!(state.buffered_entries(), 0);
        assert_eq!(state.buffer_bytes(), 0);
        assert!(!state.has_data());
        assert!(!state.needs_flush());
    }

    #[test]
    fn push_when_disabled_is_noop() {
        let mut state = TracingState::new(1, ENTRY_SIZE);
        let needs = state.push_entry(&entry(0xAA));
        assert!(!needs);
        assert_eq!(state.total_entries(), 0);
        assert_eq!(state.buffer_bytes(), 0);
    }

    #[test]
    fn enabling_resets_counters() {
        let mut state = TracingState::new(1, ENTRY_SIZE);
        state.set_enabled(true);
        state.push_entry(&entry(0x11));
        state.push_entry(&entry(0x22));
        assert_eq!(state.total_entries(), 2);
        state.set_enabled(false);
        // Re-enabling resets total + buffer position.
        state.set_enabled(true);
        assert_eq!(state.total_entries(), 0);
        assert_eq!(state.buffer_bytes(), 0);
        assert!(!state.needs_flush());
    }

    #[test]
    fn push_accumulates_bytes_in_order() {
        let mut state = TracingState::new(1, ENTRY_SIZE);
        state.set_enabled(true);
        state.push_entry(&entry(0x11));
        state.push_entry(&entry(0x22));
        let data = state.get_flush_data();
        assert_eq!(data.len(), 2 * ENTRY_SIZE);
        assert_eq!(&data[..ENTRY_SIZE], &entry(0x11));
        assert_eq!(&data[ENTRY_SIZE..], &entry(0x22));
        assert_eq!(state.buffered_entries(), 2);
    }

    #[test]
    fn clear_flushed_advances_counters_and_resets_cursor() {
        let mut state = TracingState::new(1, ENTRY_SIZE);
        state.set_enabled(true);
        state.push_entry(&entry(0xAA));
        state.push_entry(&entry(0xBB));
        state.clear_flushed();
        assert_eq!(state.flushed_entries(), 2);
        assert_eq!(state.buffered_entries(), 0);
        assert_eq!(state.buffer_bytes(), 0);
        assert!(!state.needs_flush());
        // Total stays.
        assert_eq!(state.total_entries(), 2);
    }

    #[test]
    fn buffer_doubles_when_full_below_cap() {
        // 1 MiB / 8 bytes = 131072 entries before the first growth.
        let mut state = TracingState::new(1, ENTRY_SIZE);
        state.set_enabled(true);
        let initial_capacity = state.buffer_capacity();
        for _ in 0..(initial_capacity / ENTRY_SIZE) {
            state.push_entry(&entry(0x55));
        }
        // The next push should trigger a doubling.
        state.push_entry(&entry(0x66));
        assert!(state.buffer_capacity() >= initial_capacity * 2);
        assert!(state.needs_flush());
    }

    #[test]
    fn stats_snapshot_mirrors_state() {
        let mut state = TracingState::new(1, ENTRY_SIZE);
        state.set_enabled(true);
        state.push_entry(&entry(0x42));
        let stats = TraceStats::from_state(&state);
        assert_eq!(stats.total_entries, 1);
        assert_eq!(stats.buffered_entries, 1);
        assert_eq!(stats.buffer_bytes, ENTRY_SIZE);
        assert!(stats.enabled);
    }
}
