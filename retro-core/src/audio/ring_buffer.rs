//! Bounded ring buffer used between audio producer (emulator core)
//! and consumer (host audio thread / wasm worklet).
//!
//! The buffer is **persistent**: it is *not* cleared between frames.
//! Samples produced this frame and not yet pulled by the host stay
//! queued for next frame's pull. This prevents the "samples lost
//! across frame boundary" bug where naively clearing the buffer
//! every frame drops the tail of generated audio and audibly clicks.
//!
//! When the producer pushes more than `capacity` samples without
//! the consumer draining, the buffer drops samples from the **head**
//! (oldest first) and increments `overflow_count`. When the consumer
//! tries to drain more samples than are available, `underrun_count`
//! is incremented and the rest of the consumer's destination slice
//! is filled with `T::default()`. Both counters are exposed for
//! diagnostics so the host can detect drift between the two sides
//! and surface it in the audio debugger.

use std::collections::VecDeque;

/// Bounded FIFO with overflow/underrun counters.
///
/// `T` is typically `f32` for audio samples, but kept generic so the
/// same primitive can be reused for stereo `(f32, f32)` or `i16`
/// buffers when needed.
#[derive(Debug, Clone)]
pub struct AudioRingBuffer<T: Copy + Default> {
    queue: VecDeque<T>,
    capacity: usize,
    overflow_count: u64,
    underrun_count: u64,
}

impl<T: Copy + Default> AudioRingBuffer<T> {
    /// Create a buffer with the given capacity in elements. `capacity`
    /// must be `> 0`.
    pub fn with_capacity(capacity: usize) -> Self {
        assert!(capacity > 0, "AudioRingBuffer capacity must be > 0");
        Self {
            queue: VecDeque::with_capacity(capacity),
            capacity,
            overflow_count: 0,
            underrun_count: 0,
        }
    }

    /// Maximum number of elements the buffer can hold without
    /// overflowing.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Current number of buffered elements available for draining.
    pub fn available(&self) -> usize {
        self.queue.len()
    }

    /// Number of slots free for pushing without overflow.
    pub fn free(&self) -> usize {
        self.capacity - self.queue.len()
    }

    /// Push every element of `samples` onto the tail. If the push
    /// would exceed `capacity`, the oldest elements are dropped from
    /// the head (FIFO) and `overflow_count` is bumped by the number
    /// of dropped elements. Returns the number of elements actually
    /// pushed (== `samples.len()`).
    pub fn push_slice(&mut self, samples: &[T]) -> usize {
        if samples.is_empty() {
            return 0;
        }
        if samples.len() >= self.capacity {
            // Worst case: incoming chunk is itself larger than
            // capacity. Drop everything currently buffered plus the
            // prefix of `samples` that won't fit.
            let dropped = self.queue.len() + (samples.len() - self.capacity);
            self.overflow_count = self.overflow_count.saturating_add(dropped as u64);
            self.queue.clear();
            let tail = &samples[samples.len() - self.capacity..];
            self.queue.extend(tail.iter().copied());
            return samples.len();
        }

        let new_total = self.queue.len() + samples.len();
        if new_total > self.capacity {
            let to_drop = new_total - self.capacity;
            self.overflow_count = self.overflow_count.saturating_add(to_drop as u64);
            for _ in 0..to_drop {
                let _ = self.queue.pop_front();
            }
        }
        self.queue.extend(samples.iter().copied());
        samples.len()
    }

    /// Drain up to `dst.len()` elements from the head into `dst`.
    /// Returns the number of elements actually drained. If
    /// `dst.len() > available()`, the missing slots are filled with
    /// `T::default()` and `underrun_count` is bumped by the number
    /// of missing samples.
    pub fn drain_into(&mut self, dst: &mut [T]) -> usize {
        let want = dst.len();
        let have = self.queue.len().min(want);
        for slot in dst.iter_mut().take(have) {
            *slot = self.queue.pop_front().expect("checked have <= queue.len");
        }
        if want > have {
            let missing = want - have;
            self.underrun_count = self.underrun_count.saturating_add(missing as u64);
            for slot in dst.iter_mut().skip(have) {
                *slot = T::default();
            }
        }
        have
    }

    /// Copy the **most recent** elements into `dst` *without* consuming
    /// them, right-aligned so the newest sample lands in the last slot
    /// of `dst`. If fewer than `dst.len()` elements are buffered, the
    /// leading slots are filled with `T::default()`. Returns the number
    /// of real (non-padding) elements written.
    ///
    /// Unlike [`Self::drain_into`], this leaves the queue untouched, so
    /// the audio debugger can visualise recent output without stealing
    /// samples from the host drain.
    pub fn peek_recent(&self, dst: &mut [T]) -> usize {
        let want = dst.len();
        let have = self.queue.len().min(want);
        let pad = want - have;
        for slot in dst.iter_mut().take(pad) {
            *slot = T::default();
        }
        let start = self.queue.len() - have;
        for (i, slot) in dst.iter_mut().skip(pad).enumerate() {
            *slot = self.queue[start + i];
        }
        have
    }

    /// Number of samples dropped from the head because the producer
    /// outran the consumer.
    pub fn overflow_count(&self) -> u64 {
        self.overflow_count
    }

    /// Number of "silence" samples written into a drain destination
    /// because the consumer outran the producer.
    pub fn underrun_count(&self) -> u64 {
        self.underrun_count
    }

    /// Zero both `overflow_count` and `underrun_count`. The buffered
    /// samples are *not* cleared (use [`Self::clear`] for that).
    pub fn reset_counters(&mut self) {
        self.overflow_count = 0;
        self.underrun_count = 0;
    }

    /// Drop all buffered samples (does not reset counters).
    pub fn clear(&mut self) {
        self.queue.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_capacity_initializes_empty() {
        let buf: AudioRingBuffer<f32> = AudioRingBuffer::with_capacity(4);
        assert_eq!(buf.capacity(), 4);
        assert_eq!(buf.available(), 0);
        assert_eq!(buf.free(), 4);
        assert_eq!(buf.overflow_count(), 0);
        assert_eq!(buf.underrun_count(), 0);
    }

    #[test]
    #[should_panic(expected = "AudioRingBuffer capacity must be > 0")]
    fn with_capacity_rejects_zero() {
        let _: AudioRingBuffer<f32> = AudioRingBuffer::with_capacity(0);
    }

    #[test]
    fn push_and_drain_round_trip() {
        let mut buf = AudioRingBuffer::<f32>::with_capacity(8);
        let pushed = buf.push_slice(&[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(pushed, 4);
        assert_eq!(buf.available(), 4);

        let mut out = [0.0f32; 4];
        let drained = buf.drain_into(&mut out);
        assert_eq!(drained, 4);
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(buf.available(), 0);
        assert_eq!(buf.overflow_count(), 0);
        assert_eq!(buf.underrun_count(), 0);
    }

    #[test]
    fn push_slice_empty_is_noop() {
        let mut buf = AudioRingBuffer::<f32>::with_capacity(4);
        assert_eq!(buf.push_slice(&[]), 0);
        assert_eq!(buf.available(), 0);
    }

    #[test]
    fn overflow_drops_oldest_samples() {
        let mut buf = AudioRingBuffer::<f32>::with_capacity(4);
        buf.push_slice(&[1.0, 2.0, 3.0, 4.0]);
        // Now full. Push 2 more -> drops 1.0, 2.0 from the head.
        buf.push_slice(&[5.0, 6.0]);
        assert_eq!(buf.available(), 4);
        assert_eq!(buf.overflow_count(), 2);

        let mut out = [0.0f32; 4];
        buf.drain_into(&mut out);
        assert_eq!(out, [3.0, 4.0, 5.0, 6.0]);
    }

    #[test]
    fn overflow_handles_oversized_chunk() {
        let mut buf = AudioRingBuffer::<f32>::with_capacity(3);
        buf.push_slice(&[1.0, 2.0]);
        // Push a 5-element chunk into a 3-capacity buffer:
        // all current contents are dropped + first 2 elements of new
        // chunk are dropped; only last 3 survive.
        buf.push_slice(&[10.0, 20.0, 30.0, 40.0, 50.0]);
        assert_eq!(buf.available(), 3);
        // 2 currently buffered + (5 - 3) = 4 dropped total
        assert_eq!(buf.overflow_count(), 4);

        let mut out = [0.0f32; 3];
        buf.drain_into(&mut out);
        assert_eq!(out, [30.0, 40.0, 50.0]);
    }

    #[test]
    fn underrun_fills_missing_with_default_and_bumps_counter() {
        let mut buf = AudioRingBuffer::<f32>::with_capacity(8);
        buf.push_slice(&[1.0, 2.0]);

        let mut out = [9.0f32; 5];
        let drained = buf.drain_into(&mut out);
        assert_eq!(drained, 2);
        assert_eq!(out[0], 1.0);
        assert_eq!(out[1], 2.0);
        assert_eq!(out[2], 0.0);
        assert_eq!(out[3], 0.0);
        assert_eq!(out[4], 0.0);
        assert_eq!(buf.underrun_count(), 3);
    }

    #[test]
    fn drain_into_empty_dst_is_noop() {
        let mut buf = AudioRingBuffer::<f32>::with_capacity(4);
        buf.push_slice(&[1.0, 2.0]);
        let mut out: [f32; 0] = [];
        let drained = buf.drain_into(&mut out);
        assert_eq!(drained, 0);
        assert_eq!(buf.available(), 2);
        assert_eq!(buf.underrun_count(), 0);
    }

    #[test]
    fn drain_partial_keeps_remainder() {
        let mut buf = AudioRingBuffer::<f32>::with_capacity(8);
        buf.push_slice(&[1.0, 2.0, 3.0, 4.0]);
        let mut out = [0.0f32; 2];
        buf.drain_into(&mut out);
        assert_eq!(out, [1.0, 2.0]);
        assert_eq!(buf.available(), 2);
        // Drain the rest
        let mut out2 = [0.0f32; 2];
        buf.drain_into(&mut out2);
        assert_eq!(out2, [3.0, 4.0]);
        assert_eq!(buf.available(), 0);
    }

    #[test]
    fn reset_counters_keeps_buffered_samples() {
        let mut buf = AudioRingBuffer::<f32>::with_capacity(2);
        buf.push_slice(&[1.0, 2.0, 3.0]); // 1 overflow
        let mut sink = [0.0f32; 5];
        buf.drain_into(&mut sink); // 3 underruns
        assert!(buf.overflow_count() > 0);
        assert!(buf.underrun_count() > 0);
        buf.push_slice(&[9.0]);
        buf.reset_counters();
        assert_eq!(buf.overflow_count(), 0);
        assert_eq!(buf.underrun_count(), 0);
        assert_eq!(buf.available(), 1);
    }

    #[test]
    fn clear_drops_samples_keeps_counters() {
        let mut buf = AudioRingBuffer::<f32>::with_capacity(2);
        buf.push_slice(&[1.0, 2.0, 3.0]); // overflow=1
        assert_eq!(buf.overflow_count(), 1);
        buf.clear();
        assert_eq!(buf.available(), 0);
        assert_eq!(buf.overflow_count(), 1);
    }

    #[test]
    fn generic_over_default_copy_types() {
        let mut buf = AudioRingBuffer::<i16>::with_capacity(3);
        buf.push_slice(&[10i16, 20, 30]);
        let mut out = [0i16; 3];
        buf.drain_into(&mut out);
        assert_eq!(out, [10, 20, 30]);
    }

    #[test]
    fn peek_recent_returns_newest_right_aligned_without_consuming() {
        let mut buf = AudioRingBuffer::<i16>::with_capacity(8);
        buf.push_slice(&[1, 2, 3, 4, 5]);
        let mut out = [0i16; 3];
        let real = buf.peek_recent(&mut out);
        assert_eq!(real, 3);
        // Newest three samples, oldest-to-newest.
        assert_eq!(out, [3, 4, 5]);
        // Non-consuming: the queue is untouched.
        assert_eq!(buf.available(), 5);
    }

    #[test]
    fn peek_recent_pads_leading_slots_when_underfilled() {
        let mut buf = AudioRingBuffer::<i16>::with_capacity(8);
        buf.push_slice(&[7, 8]);
        let mut out = [9i16; 5];
        let real = buf.peek_recent(&mut out);
        assert_eq!(real, 2);
        // Leading slots default-padded, newest right-aligned.
        assert_eq!(out, [0, 0, 0, 7, 8]);
        assert_eq!(buf.available(), 2);
    }

    #[test]
    fn peek_recent_empty_dst_is_noop() {
        let mut buf = AudioRingBuffer::<i16>::with_capacity(4);
        buf.push_slice(&[1, 2]);
        let mut out: [i16; 0] = [];
        assert_eq!(buf.peek_recent(&mut out), 0);
        assert_eq!(buf.available(), 2);
    }

    #[test]
    fn many_pushes_no_growth_beyond_capacity() {
        let mut buf = AudioRingBuffer::<f32>::with_capacity(64);
        for chunk in 0..1000 {
            let data: Vec<f32> = (0..16).map(|i| (chunk * 16 + i) as f32).collect();
            buf.push_slice(&data);
            assert!(buf.available() <= 64);
        }
        assert_eq!(buf.available(), 64);
    }
}
