//! Event breakpoints: the catalogue a console offers, the events its
//! hardware raises, and the params a breakpoint watches.
//!
//! A console numbers its events with codes users store in their breakpoint
//! files, so a code is never renumbered; beside each code it names the
//! event's [`EventKind`], which every console shares. The console lists its
//! events once as [`EventSpec`]s; [`event_catalog`] turns that list into the
//! catalogue a host shows, and [`check_catalogue`] is the test a console's
//! list passes.
//!
//! The hardware raises events as values of a console's own type that
//! implements [`DebugEvent`]. A chip that raises them as it works (a video
//! chip writing its palette) keeps them in a [`RaisedEvents`] until the
//! debugger drains it after the instruction, and asks only for the events a
//! breakpoint watches, from a [`ParamSet`]
//! ([`crate::debug::BreakpointList::watched_params`]).

use std::collections::HashSet;
use std::hash::{BuildHasherDefault, Hash, Hasher};

use crate::hash::{FNV_OFFSET_BASIS_64, FNV_PRIME_64};
use crate::types::{EventClass, EventKind, EventParamOption, BREAKPOINT_PARAM_ANY};

/// What an event's param is, for the catalogue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventParam {
    /// The event carries no param; a breakpoint stores
    /// [`BREAKPOINT_PARAM_ANY`] or 0.
    None,
    /// A number a user types in; `label` says what it is ("Line", "Tile").
    Number {
        /// What the number is.
        label: &'static str,
    },
    /// One of a few values, each with a name, offered as a list.
    Choice {
        /// What the value is.
        label: &'static str,
        /// The values and their names.
        options: &'static [(u32, &'static str)],
    },
}

impl EventParam {
    /// The param's label, `None` when there is no param.
    pub const fn label(&self) -> Option<&'static str> {
        match *self {
            Self::None => None,
            Self::Number { label } | Self::Choice { label, .. } => Some(label),
        }
    }
}

/// One event a console's breakpoints can stop on: its code, what it is, and
/// what the catalogue says of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventSpec {
    /// The code a breakpoint stores; never renumbered.
    pub code: u32,
    /// What the event is.
    pub kind: EventKind,
    /// A short name ("VBlank", "Palette write").
    pub label: &'static str,
    /// What the event fires on.
    pub description: &'static str,
    /// What its param is.
    pub param: EventParam,
}

/// The catalogue a host lists, one [`EventClass`] for each spec, in order.
pub fn event_catalog(specs: &[EventSpec]) -> Vec<EventClass> {
    specs
        .iter()
        .map(|spec| EventClass {
            code: spec.code,
            kind: spec.kind,
            label: spec.label.to_string(),
            description: Some(spec.description.to_string()),
            param_label: spec.param.label().map(str::to_string),
            param_options: match spec.param {
                EventParam::Choice { options, .. } => options
                    .iter()
                    .map(|&(value, label)| EventParamOption {
                        value,
                        label: label.to_string(),
                    })
                    .collect(),
                EventParam::None | EventParam::Number { .. } => Vec::new(),
            },
        })
        .collect()
}

/// Whether a console's catalogue holds together: every code once, no code
/// [`BREAKPOINT_PARAM_ANY`], a label and a description on each event, a
/// param on exactly the kinds that carry one ([`EventKind::takes_param`])
/// with a label, and each list of choices non-empty with every value once
/// and none of them [`BREAKPOINT_PARAM_ANY`]. The error names the first
/// event that does not.
pub fn check_catalogue(specs: &[EventSpec]) -> Result<(), String> {
    let mut codes = HashSet::new();
    for spec in specs {
        let named = format!("event {} ({:?})", spec.code, spec.label);
        if spec.code == BREAKPOINT_PARAM_ANY {
            return Err(format!("{named}: the code is the wildcard param"));
        }
        if !codes.insert(spec.code) {
            return Err(format!("{named}: the code is used twice"));
        }
        if spec.label.trim().is_empty() || spec.description.trim().is_empty() {
            return Err(format!("{named}: a label and a description are needed"));
        }
        let has_param = !matches!(spec.param, EventParam::None);
        if has_param != spec.kind.takes_param() {
            return Err(format!(
                "{named}: a {:?} event {} a param",
                spec.kind,
                if spec.kind.takes_param() {
                    "needs"
                } else {
                    "takes no"
                }
            ));
        }
        if spec
            .param
            .label()
            .is_some_and(|label| label.trim().is_empty())
        {
            return Err(format!("{named}: the param has no label"));
        }
        if let EventParam::Choice { options, .. } = spec.param {
            if options.is_empty() {
                return Err(format!("{named}: a choice with no options"));
            }
            let mut values = HashSet::new();
            for &(value, label) in options {
                if value == BREAKPOINT_PARAM_ANY {
                    return Err(format!("{named}: an option is the wildcard param"));
                }
                if !values.insert(value) {
                    return Err(format!("{named}: the option {value} is listed twice"));
                }
                if label.trim().is_empty() {
                    return Err(format!("{named}: the option {value} has no name"));
                }
            }
        }
    }
    Ok(())
}

/// An event a console's hardware raised, as the `(code, param)` pair an
/// event breakpoint matches. An event with no param reports 0.
pub trait DebugEvent: Copy {
    /// The event's code and param.
    fn code_param(self) -> (u32, u32);
}

/// A set of params a breakpoint watches, one bit each, `64 * WORDS` of
/// them (0 to `CAPACITY - 1`). A chip asks it whether to raise an event
/// before it does the work of raising it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ParamSet<const WORDS: usize> {
    bits: [u64; WORDS],
}

impl<const WORDS: usize> Default for ParamSet<WORDS> {
    fn default() -> Self {
        Self { bits: [0; WORDS] }
    }
}

impl<const WORDS: usize> ParamSet<WORDS> {
    /// How many params the set can hold.
    pub const CAPACITY: usize = 64 * WORDS;

    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add `param`: [`BREAKPOINT_PARAM_ANY`] adds every param; a param past
    /// the capacity adds nothing.
    pub fn insert(&mut self, param: u32) {
        if param == BREAKPOINT_PARAM_ANY {
            self.bits.fill(u64::MAX);
        } else if (param as usize) < Self::CAPACITY {
            self.bits[param as usize / 64] |= 1 << (param % 64);
        }
    }

    /// Whether `param` is in the set; a param past the capacity never is.
    #[inline]
    pub fn contains(&self, param: u32) -> bool {
        self.bits
            .get(param as usize / 64)
            .is_some_and(|word| word & (1 << (param % 64)) != 0)
    }

    /// Whether the set holds no param.
    pub fn is_empty(&self) -> bool {
        self.bits.iter().all(|&word| word == 0)
    }
}

/// The events a chip raised since the debugger last drained them. Each
/// event is kept once between two drains, in the order it was first
/// raised; past the capacity an event is not kept but counted
/// ([`Self::dropped`]). Whether an event is already kept is found without
/// looking through the ones kept, so a transfer raising thousands of events
/// in one instruction costs each a lookup.
#[derive(Debug, Clone)]
pub struct RaisedEvents<E: Copy + Eq + Hash> {
    events: Vec<E>,
    kept: HashSet<E, BuildHasherDefault<EventHasher>>,
    capacity: usize,
    dropped: u32,
}

impl<E: Copy + Eq + Hash> RaisedEvents<E> {
    /// No events, keeping at most `capacity` between two drains.
    pub fn new(capacity: usize) -> Self {
        Self {
            events: Vec::new(),
            kept: HashSet::default(),
            capacity,
            dropped: 0,
        }
    }

    /// Keep `event`, unless it is kept already; past the capacity it is
    /// counted instead.
    #[inline]
    pub fn raise(&mut self, event: E) {
        if self.kept.contains(&event) {
            return;
        }
        if self.events.len() >= self.capacity {
            self.dropped = self.dropped.saturating_add(1);
            return;
        }
        self.kept.insert(event);
        self.events.push(event);
    }

    /// Hand each kept event to `each`, in the order they were raised, and
    /// keep none: each can be raised again.
    pub fn drain(&mut self, mut each: impl FnMut(E)) {
        for event in self.events.drain(..) {
            each(event);
        }
        self.kept.clear();
    }

    /// Forget the kept events and the count of those dropped.
    pub fn clear(&mut self) {
        self.events.clear();
        self.kept.clear();
        self.dropped = 0;
    }

    /// Whether no event is kept.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// How many events are kept.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// How many events were not kept for want of room, since the set was
    /// made or last cleared (a drain does not reset it).
    pub fn dropped(&self) -> u32 {
        self.dropped
    }
}

/// The hash [`RaisedEvents`] finds its events by: FNV-1a over the bytes the
/// event hashes, the same every run.
#[derive(Debug, Clone, Copy)]
struct EventHasher(u64);

impl Default for EventHasher {
    fn default() -> Self {
        Self(FNV_OFFSET_BASIS_64)
    }
}

impl Hasher for EventHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(FNV_PRIME_64);
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PORTS: &[(u32, &str)] = &[(0, "Port 0"), (1, "Port 1")];

    fn spec(code: u32, kind: EventKind, param: EventParam) -> EventSpec {
        EventSpec {
            code,
            kind,
            label: "An event",
            description: "What it fires on",
            param,
        }
    }

    fn good_catalogue() -> [EventSpec; 3] {
        [
            spec(1, EventKind::VerticalBlank, EventParam::None),
            spec(
                2,
                EventKind::PaletteWrite,
                EventParam::Number { label: "Colour" },
            ),
            spec(
                3,
                EventKind::AudioPortWrite,
                EventParam::Choice {
                    label: "Port",
                    options: PORTS,
                },
            ),
        ]
    }

    #[test]
    fn the_catalogue_is_one_class_for_each_spec() {
        let catalogue = event_catalog(&good_catalogue());
        let json = serde_json::to_value(&catalogue).unwrap();
        assert_eq!(
            json,
            serde_json::json!([
                {
                    "code": 1, "kind": "vertical_blank", "label": "An event",
                    "description": "What it fires on", "param_label": null,
                    "param_options": []
                },
                {
                    "code": 2, "kind": "palette_write", "label": "An event",
                    "description": "What it fires on", "param_label": "Colour",
                    "param_options": []
                },
                {
                    "code": 3, "kind": "audio_port_write", "label": "An event",
                    "description": "What it fires on", "param_label": "Port",
                    "param_options": [
                        { "value": 0, "label": "Port 0" },
                        { "value": 1, "label": "Port 1" }
                    ]
                }
            ])
        );
        assert!(event_catalog(&[]).is_empty());
    }

    #[test]
    fn the_kinds_are_named_in_snake_case() {
        let names: Vec<String> = [
            EventKind::VerticalBlank,
            EventKind::VerticalBlankInterrupt,
            EventKind::RasterInterrupt,
            EventKind::Line,
            EventKind::TransferStart,
            EventKind::TransferDone,
            EventKind::LineTransferSetUp,
            EventKind::PaletteWrite,
            EventKind::TileWrite,
            EventKind::SpriteTableWrite,
            EventKind::VideoRegisterWrite,
            EventKind::AudioPortWrite,
            EventKind::AudioRegisterWrite,
        ]
        .iter()
        .map(|kind| serde_json::to_string(kind).unwrap())
        .collect();
        assert_eq!(
            names,
            [
                "\"vertical_blank\"",
                "\"vertical_blank_interrupt\"",
                "\"raster_interrupt\"",
                "\"line\"",
                "\"transfer_start\"",
                "\"transfer_done\"",
                "\"line_transfer_set_up\"",
                "\"palette_write\"",
                "\"tile_write\"",
                "\"sprite_table_write\"",
                "\"video_register_write\"",
                "\"audio_port_write\"",
                "\"audio_register_write\"",
            ]
        );
    }

    #[test]
    fn a_good_catalogue_passes_its_check() {
        assert_eq!(check_catalogue(&good_catalogue()), Ok(()));
        assert_eq!(check_catalogue(&[]), Ok(()));
    }

    #[test]
    fn a_catalogue_that_does_not_hold_together_fails_its_check() {
        let none = EventParam::None;
        let number = EventParam::Number { label: "Line" };
        let bad: [(&str, Vec<EventSpec>); 9] = [
            (
                "a code used twice",
                vec![
                    spec(1, EventKind::VerticalBlank, none),
                    spec(1, EventKind::Line, number),
                ],
            ),
            (
                "the wildcard as a code",
                vec![spec(BREAKPOINT_PARAM_ANY, EventKind::VerticalBlank, none)],
            ),
            (
                "a param on a kind that takes none",
                vec![spec(1, EventKind::VerticalBlank, number)],
            ),
            (
                "no param on a kind that takes one",
                vec![spec(1, EventKind::TileWrite, none)],
            ),
            (
                "a param with no label",
                vec![spec(1, EventKind::Line, EventParam::Number { label: " " })],
            ),
            (
                "a choice of nothing",
                vec![spec(
                    1,
                    EventKind::TransferDone,
                    EventParam::Choice {
                        label: "Memory",
                        options: &[],
                    },
                )],
            ),
            (
                "an option listed twice",
                vec![spec(
                    1,
                    EventKind::TransferDone,
                    EventParam::Choice {
                        label: "Memory",
                        options: &[(0, "VRAM"), (0, "CRAM")],
                    },
                )],
            ),
            (
                "the wildcard as an option",
                vec![spec(
                    1,
                    EventKind::TransferDone,
                    EventParam::Choice {
                        label: "Memory",
                        options: &[(BREAKPOINT_PARAM_ANY, "Any")],
                    },
                )],
            ),
            (
                "an event with no label",
                vec![EventSpec {
                    label: "",
                    ..spec(1, EventKind::VerticalBlank, none)
                }],
            ),
        ];
        for (what, specs) in bad {
            assert!(check_catalogue(&specs).is_err(), "{what} passed the check");
        }
    }

    #[test]
    fn a_param_set_holds_what_was_inserted() {
        let mut set = ParamSet::<1>::new();
        assert!(set.is_empty());
        set.insert(0);
        set.insert(63);
        assert!(set.contains(0) && set.contains(63));
        assert!(!set.contains(1));
        assert!(!set.is_empty());
        assert_eq!(ParamSet::<1>::CAPACITY, 64);
        assert_eq!(ParamSet::<32>::CAPACITY, 2048);
    }

    #[test]
    fn a_param_past_the_capacity_is_neither_held_nor_inserted() {
        let mut set = ParamSet::<1>::new();
        set.insert(64);
        set.insert(5000);
        assert!(set.is_empty());
        assert!(!set.contains(64));
        let mut tiles = ParamSet::<32>::new();
        tiles.insert(2047);
        tiles.insert(2048);
        assert!(tiles.contains(2047) && !tiles.contains(2048));
    }

    #[test]
    fn the_wildcard_inserts_every_param() {
        let mut set = ParamSet::<4>::new();
        set.insert(BREAKPOINT_PARAM_ANY);
        assert!((0..256).all(|param| set.contains(param)));
        assert!(!set.contains(256), "the capacity still bounds the set");
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    enum Raised {
        Colour(u8),
        Done,
    }

    impl DebugEvent for Raised {
        fn code_param(self) -> (u32, u32) {
            match self {
                Self::Colour(colour) => (6, u32::from(colour)),
                Self::Done => (5, 0),
            }
        }
    }

    fn drained(raised: &mut RaisedEvents<Raised>) -> Vec<Raised> {
        let mut found = Vec::new();
        raised.drain(|event| found.push(event));
        found
    }

    #[test]
    fn an_event_is_kept_once_between_two_drains_in_the_order_raised() {
        let mut raised = RaisedEvents::new(16);
        assert!(raised.is_empty());
        raised.raise(Raised::Colour(3));
        raised.raise(Raised::Done);
        raised.raise(Raised::Colour(3));
        raised.raise(Raised::Colour(4));
        raised.raise(Raised::Done);
        assert_eq!(raised.len(), 3);
        assert_eq!(
            drained(&mut raised),
            [Raised::Colour(3), Raised::Done, Raised::Colour(4)]
        );
        assert!(raised.is_empty());
        raised.raise(Raised::Colour(3));
        assert_eq!(
            drained(&mut raised),
            [Raised::Colour(3)],
            "a drained event can be raised again"
        );
    }

    #[test]
    fn past_the_capacity_events_are_counted_not_kept() {
        let mut raised = RaisedEvents::new(2);
        for colour in 0..5 {
            raised.raise(Raised::Colour(colour));
        }
        raised.raise(Raised::Colour(0));
        assert_eq!(raised.len(), 2);
        assert_eq!(raised.dropped(), 3, "an event already kept is no drop");
        assert_eq!(drained(&mut raised), [Raised::Colour(0), Raised::Colour(1)]);
        assert_eq!(raised.dropped(), 3, "a drain keeps the count");
        raised.raise(Raised::Colour(9));
        raised.clear();
        assert!(raised.is_empty());
        assert_eq!(raised.dropped(), 0);
        assert!(drained(&mut raised).is_empty());
    }

    #[test]
    fn thousands_of_events_are_kept_each_once() {
        let mut raised = RaisedEvents::new(4096);
        for round in 0..3 {
            for colour in 0..=255u8 {
                raised.raise(Raised::Colour(colour));
            }
            assert_eq!(raised.len(), 256, "round {round}");
        }
        let found = drained(&mut raised);
        assert_eq!(found.len(), 256);
        assert_eq!(found[255], Raised::Colour(255));
    }

    #[test]
    fn the_event_hasher_is_the_same_every_run() {
        let hash = |event: Raised| {
            let mut hasher = EventHasher::default();
            event.hash(&mut hasher);
            hasher.finish()
        };
        assert_eq!(hash(Raised::Done), hash(Raised::Done));
        assert_ne!(hash(Raised::Colour(1)), hash(Raised::Colour(2)));
        let mut text = EventHasher::default();
        text.write(b"foo");
        text.write(b"bar");
        assert_eq!(text.finish(), crate::hash::fnv1a64(b"foobar"));
    }
}
