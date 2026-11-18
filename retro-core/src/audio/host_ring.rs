//! The samples a machine has made and the host has not taken yet.
//!
//! Every console queues the sound it mixed at a frame's end here, and the
//! host drains it at its own pace: [`AudioRingBuffer`] keeps what was not
//! taken for the next drain, drops the oldest samples when the host falls
//! behind, and counts both. The drain scratch lets
//! [`crate::EmulatorCore::get_audio_samples`] hand out a slice with no
//! allocation once warm.

use super::AudioRingBuffer;

/// Interleaved left and right `f32` samples on their way to the host.
#[derive(Debug, Clone)]
pub struct HostAudioRing {
    ring: AudioRingBuffer<f32>,
    drained: Vec<f32>,
}

impl HostAudioRing {
    /// A ring that holds at most `samples` samples (both channels counted).
    pub fn with_capacity(samples: usize) -> Self {
        Self {
            ring: AudioRingBuffer::with_capacity(samples),
            drained: Vec::with_capacity(2048),
        }
    }

    /// Queue `samples`, the oldest dropped (and counted) past the capacity.
    #[inline]
    pub fn push_slice(&mut self, samples: &[f32]) {
        self.ring.push_slice(samples);
    }

    /// Samples queued.
    pub fn available(&self) -> usize {
        self.ring.available()
    }

    /// Move up to `out.len()` queued samples into `out`, returning how many
    /// were moved; past what is queued, `out` gets silence and an underrun
    /// is counted.
    pub fn drain_into(&mut self, out: &mut [f32]) -> usize {
        self.ring.drain_into(out)
    }

    /// Every queued sample, the ring left empty.
    pub fn drain_all(&mut self) -> &[f32] {
        let n = self.ring.available();
        self.drained.clear();
        self.drained.resize(n, 0.0);
        self.ring.drain_into(&mut self.drained);
        &self.drained
    }

    /// Every queued sample in a vector of its own, the ring left empty.
    pub fn take_all(&mut self) -> Vec<f32> {
        let mut out = vec![0.0; self.ring.available()];
        self.ring.drain_into(&mut out);
        out
    }

    /// Times the host fell behind and the oldest samples were dropped.
    pub fn overflows(&self) -> u64 {
        self.ring.overflow_count()
    }

    /// Times the host asked for more samples than were queued.
    pub fn underruns(&self) -> u64 {
        self.ring.underrun_count()
    }

    /// Drop every queued sample. Not while the host plays: it would race
    /// the host's drains and click.
    pub fn clear(&mut self) {
        self.ring.clear();
    }

    /// Zero the overflow and underrun counts.
    pub fn reset_counters(&mut self) {
        self.ring.reset_counters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drains_what_was_queued_in_order() {
        let mut ring = HostAudioRing::with_capacity(8);
        ring.push_slice(&[0.1, 0.2, 0.3]);
        assert_eq!(ring.available(), 3);
        assert_eq!(ring.drain_all(), &[0.1, 0.2, 0.3]);
        assert_eq!(ring.available(), 0);
        assert!(ring.drain_all().is_empty());
        ring.push_slice(&[0.4, 0.5]);
        assert_eq!(ring.take_all(), vec![0.4, 0.5]);
    }

    #[test]
    fn counts_overflows_and_underruns() {
        let mut ring = HostAudioRing::with_capacity(4);
        ring.push_slice(&[1.0; 6]);
        assert_eq!(ring.available(), 4);
        assert!(ring.overflows() > 0);
        let mut out = [9.0; 6];
        assert_eq!(ring.drain_into(&mut out), 4);
        assert_eq!(&out[4..], &[0.0, 0.0]);
        assert!(ring.underruns() > 0);
        ring.reset_counters();
        assert_eq!((ring.overflows(), ring.underruns()), (0, 0));
        ring.push_slice(&[1.0]);
        ring.clear();
        assert_eq!(ring.available(), 0);
    }
}
