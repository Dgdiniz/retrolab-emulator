//! The maps the input tests play with: a six-button pad's (the Mega
//! Drive's layout), so every test reads the same bits.

use std::collections::HashMap;

/// Arrows the directions (bits 0-3), A S D buttons 6 4 5, Q W E 10 9 8,
/// Enter 7, left Shift 11.
pub(crate) fn keyboard() -> HashMap<String, u32> {
    [
        ("ArrowUp", 0),
        ("ArrowDown", 1),
        ("ArrowLeft", 2),
        ("ArrowRight", 3),
        ("KeyA", 6),
        ("KeyS", 4),
        ("KeyD", 5),
        ("KeyQ", 10),
        ("KeyW", 9),
        ("KeyE", 8),
        ("Enter", 7),
        ("ShiftLeft", 11),
    ]
    .into_iter()
    .map(|(key, bit)| (key.to_string(), bit))
    .collect()
}

/// Face buttons 0-3 to bits 6 4 5 10, shoulders 4 5 to 9 8, Start 9 to 7,
/// Select 8 to 11, the pad's directions 12-15 to 0-3.
pub(crate) fn gamepad() -> HashMap<u32, u32> {
    HashMap::from([
        (0, 6),
        (1, 4),
        (2, 5),
        (3, 10),
        (4, 9),
        (5, 8),
        (9, 7),
        (8, 11),
        (12, 0),
        (13, 1),
        (14, 2),
        (15, 3),
    ])
}
