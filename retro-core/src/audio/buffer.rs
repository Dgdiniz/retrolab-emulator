//! Pre-allocated scratch buffer for per-frame audio chip generation.
//!
//! Audio chips typically need a contiguous `&mut [f32]` to write the
//! samples they produced during a frame. Allocating a `Vec<f32>` of
//! a few thousand entries every frame creates measurable allocator
//! pressure in WASM builds and shows up in profiles as noise.
//!
//! `ChipSampleBuffer` is a thin wrapper around `Vec<f32>` that
//! exposes `ensure_capacity(n)` to grow the backing storage once
//! (without zeroing existing data unless it actually had to grow)
//! and `as_mut_slice(n)` to hand out a slice of exactly `n`
//! freshly-zeroed samples. The intended pattern is:
//!
//! ```ignore
//! let slice = self.scratch.as_mut_slice(samples_needed);
//! chip.generate_samples(slice, output_rate);
//! ring_buffer.push_slice(slice);
//! ```

/// Pre-allocated scratch buffer for `f32` audio samples. Holds
/// capacity across frames so generation paths can write a slice of
/// the size they need without reallocating.
#[derive(Debug, Clone, Default)]
pub struct ChipSampleBuffer {
    buf: Vec<f32>,
}

impl ChipSampleBuffer {
    /// Empty buffer. No allocation until [`Self::ensure_capacity`] or
    /// [`Self::as_mut_slice`] is called.
    pub fn new() -> Self {
        Self::default()
    }

    /// Empty buffer with `cap` slots reserved up front.
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            buf: Vec::with_capacity(cap),
        }
    }

    /// Total capacity of the underlying `Vec`.
    pub fn capacity(&self) -> usize {
        self.buf.capacity()
    }

    /// Current logical length (== last slice handed out).
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// True iff `len() == 0`.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Reserve at least `cap` slots without allocating again if we
    /// already have them. Cheap to call every frame.
    pub fn ensure_capacity(&mut self, cap: usize) {
        if cap > self.buf.capacity() {
            // `Vec::reserve(additional)` reserves enough room for
            // `len + additional` elements, so we ask for `cap - len`
            // more to make the new capacity reach at least `cap`.
            let additional = cap - self.buf.len();
            self.buf.reserve(additional);
        }
    }

    /// Return a mutable slice of exactly `n` zero-initialized samples.
    /// The internal `Vec` is resized to `n` (growing or shrinking) so
    /// the slice exactly matches the chip's expectation.
    pub fn as_mut_slice(&mut self, n: usize) -> &mut [f32] {
        self.buf.clear();
        self.buf.resize(n, 0.0);
        &mut self.buf[..]
    }

    /// Read-only view of the current contents.
    pub fn as_slice(&self) -> &[f32] {
        &self.buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_starts_empty() {
        let b = ChipSampleBuffer::new();
        assert_eq!(b.len(), 0);
        assert!(b.is_empty());
        assert_eq!(b.capacity(), 0);
    }

    #[test]
    fn with_capacity_reserves_without_allocating_per_call() {
        let b = ChipSampleBuffer::with_capacity(2048);
        assert!(b.capacity() >= 2048);
        assert_eq!(b.len(), 0);
    }

    #[test]
    fn ensure_capacity_grows_to_request_but_keeps_logical_len() {
        let mut b = ChipSampleBuffer::with_capacity(8);
        let initial_cap = b.capacity();
        b.ensure_capacity(initial_cap);
        assert_eq!(b.capacity(), initial_cap, "no-op when already big enough");

        b.ensure_capacity(initial_cap + 100);
        assert!(b.capacity() >= initial_cap + 100);
    }

    #[test]
    fn as_mut_slice_returns_zeroed_slice_of_exact_size() {
        let mut b = ChipSampleBuffer::new();
        let s = b.as_mut_slice(5);
        assert_eq!(s.len(), 5);
        for v in s.iter() {
            assert_eq!(*v, 0.0);
        }
    }

    #[test]
    fn as_mut_slice_zeroes_between_calls() {
        let mut b = ChipSampleBuffer::with_capacity(16);
        {
            let s = b.as_mut_slice(4);
            for v in s.iter_mut() {
                *v = 7.0;
            }
        }
        // Next request should be freshly zeroed, even if it overlaps.
        let s = b.as_mut_slice(4);
        for v in s.iter() {
            assert_eq!(*v, 0.0);
        }
    }

    #[test]
    fn as_mut_slice_can_shrink_and_grow() {
        let mut b = ChipSampleBuffer::with_capacity(32);
        b.as_mut_slice(20);
        assert_eq!(b.len(), 20);
        b.as_mut_slice(5);
        assert_eq!(b.len(), 5);
        b.as_mut_slice(31);
        assert_eq!(b.len(), 31);
    }

    #[test]
    fn capacity_stable_after_many_same_size_frames() {
        let mut b = ChipSampleBuffer::with_capacity(2048);
        let initial_cap = b.capacity();
        for _ in 0..1000 {
            let s = b.as_mut_slice(2048);
            assert_eq!(s.len(), 2048);
        }
        assert_eq!(
            b.capacity(),
            initial_cap,
            "no growth across stable-size frames"
        );
    }
}
