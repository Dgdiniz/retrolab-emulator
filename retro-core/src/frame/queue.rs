//! A queue of timed events: a console whose timetable is a list of raster
//! points keeps them here and handles each as the clock reaches it.

use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;

/// One queued event: its cycle, the order it was scheduled in, and what
/// it is. Ordered by `(at, seq)` alone, so the heap pops the earliest
/// cycle first and events on one cycle in the order they were scheduled.
#[derive(Debug, Clone)]
struct Scheduled<K> {
    at: u64,
    seq: u64,
    kind: K,
}

impl<K> PartialEq for Scheduled<K> {
    fn eq(&self, other: &Self) -> bool {
        (self.at, self.seq) == (other.at, other.seq)
    }
}

impl<K> Eq for Scheduled<K> {}

impl<K> PartialOrd for Scheduled<K> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<K> Ord for Scheduled<K> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.at.cmp(&other.at).then(self.seq.cmp(&other.seq))
    }
}

/// A min-heap of timed events, earliest first, events on one cycle in the
/// order they were scheduled.
///
/// The heap's storage order is what a core's savestate writes
/// ([`Self::stored`]), so the queue makes the same pushes and pops for
/// the same use: a drain takes every due event off the heap before the
/// first is handled (a handler that schedules pushes after the drain's
/// pops, never between them), and a load pushes the saved events back in
/// their stored order, which rebuilds the same storage.
#[derive(Debug, Clone)]
pub struct EventQueue<K> {
    events: BinaryHeap<Reverse<Scheduled<K>>>,
    /// The cycle the last drain reached.
    cursor: u64,
    next_seq: u64,
    /// The buffer the batches are taken into, kept between drains so a
    /// drain allocates nothing once warm.
    spare: Vec<(u64, K)>,
}

/// Events taken off the queue, in the order they fall due, to be handled
/// one by one; give the batch back to the queue after
/// ([`EventQueue::give_back`]) so its buffer is reused.
#[derive(Debug)]
pub struct DueBatch<K> {
    events: Vec<(u64, K)>,
}

impl<K: Copy> DueBatch<K> {
    /// The events with the cycle each was scheduled for, in order.
    pub fn iter(&self) -> impl Iterator<Item = (u64, K)> + '_ {
        self.events.iter().copied()
    }

    /// How many events the batch holds.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether the batch holds none.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

impl<K> Default for EventQueue<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K> EventQueue<K> {
    /// An empty queue at cycle 0.
    pub fn new() -> Self {
        Self {
            events: BinaryHeap::new(),
            cursor: 0,
            next_seq: 0,
            spare: Vec::new(),
        }
    }

    /// Empty the queue and put its cursor at `cursor`; the sequence starts
    /// again at 0.
    pub fn reset(&mut self, cursor: u64) {
        self.events.clear();
        self.cursor = cursor;
        self.next_seq = 0;
    }

    /// Queue `kind` at cycle `at`.
    pub fn schedule(&mut self, at: u64, kind: K) {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        self.events.push(Reverse(Scheduled { at, seq, kind }));
    }

    /// The cycle of the earliest queued event.
    pub fn peek_cycle(&self) -> Option<u64> {
        self.events.peek().map(|Reverse(ev)| ev.at)
    }

    /// Take every event due by `cycle` off the queue, each with the cycle
    /// it was scheduled for, the whole batch before any is handled, and
    /// move the cursor to `cycle`.
    pub fn take_due(&mut self, cycle: u64) -> DueBatch<K> {
        debug_assert!(
            cycle >= self.cursor,
            "a drain to {cycle} must not go back past the cursor {}",
            self.cursor
        );
        let mut events = std::mem::take(&mut self.spare);
        while self
            .events
            .peek()
            .is_some_and(|Reverse(top)| top.at <= cycle)
        {
            let Reverse(ev) = self.events.pop().expect("peeked above");
            events.push((ev.at, ev.kind));
        }
        self.cursor = cycle;
        DueBatch { events }
    }

    /// [`Self::take_due`], stopping in front of the first due event `stop`
    /// accepts, which stays queued with every event after it. The second
    /// value says whether it stopped there.
    pub fn take_due_before(
        &mut self,
        cycle: u64,
        stop: impl Fn(&K) -> bool,
    ) -> (DueBatch<K>, bool) {
        debug_assert!(
            cycle >= self.cursor,
            "a drain to {cycle} must not go back past the cursor {}",
            self.cursor
        );
        let mut events = std::mem::take(&mut self.spare);
        let mut stopped = false;
        while let Some(Reverse(top)) = self.events.peek() {
            if top.at > cycle {
                break;
            }
            if stop(&top.kind) {
                stopped = true;
                break;
            }
            let Reverse(ev) = self.events.pop().expect("peeked above");
            events.push((ev.at, ev.kind));
        }
        self.cursor = cycle;
        (DueBatch { events }, stopped)
    }

    /// Take a handled batch's buffer back for the next drain.
    pub fn give_back(&mut self, mut batch: DueBatch<K>) {
        batch.events.clear();
        if batch.events.capacity() >= self.spare.capacity() {
            self.spare = batch.events;
        }
    }

    /// The earliest cycle of a queued event `pred` accepts.
    pub fn earliest(&self, pred: impl Fn(&K) -> bool) -> Option<u64> {
        self.events
            .iter()
            .filter(|Reverse(ev)| pred(&ev.kind))
            .map(|Reverse(ev)| ev.at)
            .min()
    }

    /// The cycles of the queued events `pred` accepts, earliest first.
    pub fn cycles_of(&self, pred: impl Fn(&K) -> bool) -> Vec<u64> {
        let mut cycles: Vec<u64> = self
            .events
            .iter()
            .filter(|Reverse(ev)| pred(&ev.kind))
            .map(|Reverse(ev)| ev.at)
            .collect();
        cycles.sort_unstable();
        cycles
    }

    /// Drop every queued event `pred` accepts. The rest are rebuilt into a
    /// heap from their storage order, as they always have been, so the
    /// storage a savestate writes after it is the same.
    pub fn cancel_matching(&mut self, pred: impl Fn(&K) -> bool) {
        let kept: Vec<_> = self
            .events
            .drain()
            .filter(|Reverse(ev)| !pred(&ev.kind))
            .collect();
        self.events = BinaryHeap::from(kept);
    }

    /// How many events are queued.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether none is.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// The cycle the last drain reached (or the reset's).
    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    /// The sequence number the next scheduled event takes.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// The queued events as `(cycle, sequence, kind)`, in the heap's
    /// storage order: what a savestate writes.
    pub fn stored(&self) -> impl Iterator<Item = (u64, u64, &K)> {
        self.events
            .iter()
            .map(|Reverse(ev)| (ev.at, ev.seq, &ev.kind))
    }

    /// Rebuild the queue from a savestate: the cursor, the sequence, and
    /// the events in the stored order they were written in, pushed one by
    /// one, which leaves the same storage.
    pub fn restore(
        &mut self,
        cursor: u64,
        next_seq: u64,
        events: impl IntoIterator<Item = (u64, u64, K)>,
    ) {
        self.cursor = cursor;
        self.next_seq = next_seq;
        self.events.clear();
        for (at, seq, kind) in events {
            self.events.push(Reverse(Scheduled { at, seq, kind }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Ev {
        A,
        B,
        C,
        Mark(u8),
        End,
    }

    fn drained(q: &mut EventQueue<Ev>, cycle: u64) -> Vec<(u64, Ev)> {
        let batch = q.take_due(cycle);
        let out: Vec<_> = batch.iter().collect();
        q.give_back(batch);
        out
    }

    #[test]
    fn the_earliest_event_is_peeked() {
        let mut q = EventQueue::new();
        q.schedule(500, Ev::A);
        q.schedule(100, Ev::B);
        q.schedule(900, Ev::End);
        assert_eq!(q.peek_cycle(), Some(100));
        assert_eq!(q.len(), 3);
        assert_eq!(EventQueue::<Ev>::new().peek_cycle(), None);
    }

    #[test]
    fn a_drain_takes_what_is_due_in_order_with_its_cycles() {
        let mut q = EventQueue::new();
        q.schedule(500, Ev::A);
        q.schedule(100, Ev::B);
        q.schedule(300, Ev::C);
        q.schedule(900, Ev::End);
        assert_eq!(
            drained(&mut q, 600),
            vec![(100, Ev::B), (300, Ev::C), (500, Ev::A)]
        );
        assert_eq!(q.cursor(), 600);
        assert_eq!(q.len(), 1);
        assert!(drained(&mut q, 600).is_empty());
        assert_eq!(drained(&mut q, 1000), vec![(900, Ev::End)]);
        assert!(q.is_empty());
    }

    #[test]
    fn events_on_one_cycle_fall_due_in_the_order_they_were_scheduled() {
        let mut q = EventQueue::new();
        q.schedule(1000, Ev::A);
        q.schedule(1000, Ev::B);
        q.schedule(1000, Ev::C);
        assert_eq!(
            drained(&mut q, 2000),
            vec![(1000, Ev::A), (1000, Ev::B), (1000, Ev::C)]
        );
        // The sequence goes on across drains, so later ties keep their order.
        q.schedule(2100, Ev::C);
        q.schedule(2100, Ev::A);
        assert_eq!(drained(&mut q, 3000), vec![(2100, Ev::C), (2100, Ev::A)]);
    }

    #[test]
    fn an_event_after_the_drain_cycle_stays_queued() {
        let mut q = EventQueue::new();
        q.schedule(100, Ev::A);
        q.schedule(101, Ev::B);
        assert_eq!(drained(&mut q, 100), vec![(100, Ev::A)]);
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn a_drain_before_an_event_leaves_it_and_what_follows_queued() {
        let mut q = EventQueue::new();
        q.schedule(100, Ev::A);
        q.schedule(200, Ev::Mark(1));
        q.schedule(200, Ev::B);
        q.schedule(300, Ev::C);
        q.schedule(900, Ev::End);
        let (batch, stopped) = q.take_due_before(400, |k| matches!(k, Ev::Mark(_)));
        assert!(stopped);
        assert_eq!(batch.iter().collect::<Vec<_>>(), vec![(100, Ev::A)]);
        q.give_back(batch);
        assert_eq!(
            drained(&mut q, 400),
            vec![(200, Ev::Mark(1)), (200, Ev::B), (300, Ev::C)]
        );
        let (batch, stopped) = q.take_due_before(1000, |k| matches!(k, Ev::Mark(_)));
        assert!(!stopped);
        assert_eq!(batch.iter().collect::<Vec<_>>(), vec![(900, Ev::End)]);
    }

    #[test]
    fn cancelling_drops_only_what_matches() {
        let mut q = EventQueue::new();
        q.schedule(100, Ev::Mark(10));
        q.schedule(200, Ev::A);
        q.schedule(300, Ev::Mark(20));
        q.schedule(400, Ev::B);
        q.cancel_matching(|k| matches!(k, Ev::Mark(_)));
        assert_eq!(q.len(), 2);
        assert_eq!(drained(&mut q, 1000), vec![(200, Ev::A), (400, Ev::B)]);
    }

    #[test]
    fn the_cycles_of_matching_events_come_earliest_first() {
        let mut q = EventQueue::new();
        q.schedule(300, Ev::Mark(3));
        q.schedule(100, Ev::Mark(1));
        q.schedule(50, Ev::A);
        q.schedule(200, Ev::Mark(2));
        assert_eq!(
            q.cycles_of(|k| matches!(k, Ev::Mark(_))),
            vec![100, 200, 300]
        );
        assert_eq!(q.earliest(|k| matches!(k, Ev::Mark(_))), Some(100));
        assert_eq!(q.earliest(|k| *k == Ev::End), None);
        assert_eq!(q.len(), 4);
    }

    #[test]
    fn a_reset_empties_the_queue_and_moves_the_cursor() {
        let mut q = EventQueue::new();
        q.schedule(500, Ev::A);
        q.schedule(700, Ev::B);
        drained(&mut q, 100);
        q.reset(12_345);
        assert!(q.is_empty());
        assert_eq!(q.peek_cycle(), None);
        assert_eq!(q.cursor(), 12_345);
        assert_eq!(q.next_seq(), 0);
    }

    /// The storage a savestate writes depends on the order of the heap's
    /// pushes and pops, not only on what it holds: a drain that handled
    /// each event before popping the next, with a handler that schedules,
    /// would store the same set in another order. The queue takes the
    /// whole batch first, which is the order its storage has always come
    /// from.
    #[test]
    fn a_batch_taken_whole_keeps_the_heap_storage_of_a_drain() {
        let cycles = [24u64, 0, 88, 1096, 1104, 1452, 2460, 2468, 2816, 3824];
        let mut q = EventQueue::new();
        let mut reference: BinaryHeap<Reverse<(u64, u64)>> = BinaryHeap::new();
        for (seq, &at) in cycles.iter().enumerate() {
            q.schedule(at, Ev::A);
            reference.push(Reverse((at, seq as u64)));
        }
        // The queue: the whole batch, then the handler of the event at 1096
        // schedules one more.
        let batch = q.take_due(1500);
        let handled: Vec<_> = batch.iter().collect();
        q.give_back(batch);
        for &(at, _) in &handled {
            if at == 1096 {
                q.schedule(9000, Ev::B);
            }
        }
        // The same pops and pushes on a bare heap.
        let mut taken = Vec::new();
        while reference.peek().is_some_and(|Reverse((at, _))| *at <= 1500) {
            taken.push(reference.pop().unwrap().0);
        }
        reference.push(Reverse((9000, cycles.len() as u64)));
        let stored: Vec<(u64, u64)> = q.stored().map(|(at, seq, _)| (at, seq)).collect();
        let expected: Vec<(u64, u64)> = reference.iter().map(|Reverse(e)| *e).collect();
        assert_eq!(stored, expected);
        assert_eq!(handled.len(), taken.len());
    }

    #[test]
    fn a_restore_rebuilds_the_storage_it_was_saved_from() {
        let mut q = EventQueue::new();
        for (i, at) in [700u64, 100, 300, 300, 50, 900, 20].into_iter().enumerate() {
            q.schedule(at, Ev::Mark(i as u8));
        }
        drained(&mut q, 60);
        q.schedule(65, Ev::C);
        let saved: Vec<(u64, u64, Ev)> = q.stored().map(|(at, seq, k)| (at, seq, *k)).collect();
        let mut loaded = EventQueue::new();
        loaded.restore(q.cursor(), q.next_seq(), saved.clone());
        let again: Vec<(u64, u64, Ev)> =
            loaded.stored().map(|(at, seq, k)| (at, seq, *k)).collect();
        assert_eq!(again, saved);
        assert_eq!(loaded.cursor(), q.cursor());
        assert_eq!(loaded.next_seq(), q.next_seq());
    }
}
