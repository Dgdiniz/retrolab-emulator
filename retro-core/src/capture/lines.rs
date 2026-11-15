//! The line table's helpers: the palettes the lines were drawn with, and the
//! machinery that turns a line's settings into the columns of a
//! [`crate::video::LineStateTable`].

use std::collections::HashMap;

/// The palette index of a line nothing was recorded of, in
/// [`LinePalettes::palette_of_line`] and in a line table's `palette` column.
pub const NO_PALETTE: u16 = 0xFFFF;

/// The column a palette write coloured its line again from, in a core's
/// [`LinePalettes::changes`], when it coloured nothing again.
pub const NO_COLUMN: u16 = 0xFFFF;

/// The palettes the lines of the picture were drawn with, each once, as the
/// line table names them ([`super::CaptureCore::line_palettes`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinePalettes {
    /// The colours of each palette: the whole palette memory (64 on the Mega
    /// Drive, 256 on the SNES).
    pub colours_per_palette: u32,
    /// Each palette a line was drawn with, once, in the order the lines first
    /// used it: `colours_per_palette` colours each, in the console's own
    /// colour words.
    pub palettes: Vec<u16>,
    /// For each row of the line table, the palette its line was drawn with,
    /// an index into `palettes`, or [`NO_PALETTE`] for a line nothing was
    /// recorded of.
    pub palette_of_line: Vec<u16>,
    /// The palette writes made while each line was on screen, row by row and
    /// in order, in the core's own layout (the Mega Drive's is five values a
    /// write), where the core records them; `None` where it records none.
    pub changes: Option<Vec<u16>>,
}

/// Each distinct palette the lines were drawn with, indexed once in the
/// order the lines first used it, and each line's index into them: one item
/// of `lines` a line, its palette of `N` colours, or `None` for a line
/// nothing was recorded of ([`NO_PALETTE`]). `changes` is left `None`, for
/// the core to fill.
pub fn distinct_palettes<const N: usize>(
    lines: impl IntoIterator<Item = Option<[u16; N]>>,
) -> LinePalettes {
    let lines = lines.into_iter();
    let mut index_of: HashMap<[u16; N], u16> = HashMap::new();
    let mut palettes: Vec<u16> = Vec::new();
    let mut palette_of_line = Vec::with_capacity(lines.size_hint().0);
    for palette in lines {
        let Some(palette) = palette else {
            palette_of_line.push(NO_PALETTE);
            continue;
        };
        let next = index_of.len() as u16;
        let index = *index_of.entry(palette).or_insert_with(|| {
            palettes.extend_from_slice(&palette);
            next
        });
        palette_of_line.push(index);
    }
    LinePalettes {
        colours_per_palette: N as u32,
        palettes,
        palette_of_line,
        changes: None,
    }
}

/// Writes one setting of a line as columns of a line table: a name and a
/// value, or a name and a value for each element of a setting kept per
/// layer (`name[index]`), or for each part of a setting a core breaks into
/// parts.
pub trait TableColumns {
    /// Push the names of the columns of a setting called `name`.
    fn push_names(name: &str, names: &mut Vec<String>);
    /// Push the values, one per column, in the order of the names.
    fn push_values(&self, values: &mut Vec<u32>);
}

impl TableColumns for bool {
    fn push_names(name: &str, names: &mut Vec<String>) {
        names.push(name.to_string());
    }
    fn push_values(&self, values: &mut Vec<u32>) {
        values.push(u32::from(*self));
    }
}

impl TableColumns for u8 {
    fn push_names(name: &str, names: &mut Vec<String>) {
        names.push(name.to_string());
    }
    fn push_values(&self, values: &mut Vec<u32>) {
        values.push(u32::from(*self));
    }
}

impl TableColumns for u16 {
    fn push_names(name: &str, names: &mut Vec<String>) {
        names.push(name.to_string());
    }
    fn push_values(&self, values: &mut Vec<u32>) {
        values.push(u32::from(*self));
    }
}

impl TableColumns for u32 {
    fn push_names(name: &str, names: &mut Vec<String>) {
        names.push(name.to_string());
    }
    fn push_values(&self, values: &mut Vec<u32>) {
        values.push(*self);
    }
}

impl<T: TableColumns, const N: usize> TableColumns for [T; N] {
    fn push_names(name: &str, names: &mut Vec<String>) {
        for index in 0..N {
            T::push_names(&format!("{name}[{index}]"), names);
        }
    }
    fn push_values(&self, values: &mut Vec<u32>) {
        for element in self {
            element.push_values(values);
        }
    }
}

/// Declares the settings one line of the picture was drawn with, from one
/// list, each setting named as the line table names it and paired with the
/// field of the chip that stores it, so that taking a line's settings,
/// putting them back and writing them as columns can never disagree on what
/// a line holds.
///
/// The struct gets the attributes and the visibility written before it, and
/// one public field a setting; each setting's type implements
/// [`TableColumns`](crate::capture::TableColumns) and `Copy`. It gets:
///
/// - `capture(chip: &Chip) -> Self` (crate-visible): the settings as the chip
///   holds them now;
/// - `apply(&self, chip: &mut Chip)` (crate-visible): put them back into the
///   chip, as they were when taken;
/// - `column_names() -> Vec<String>`: the names of the columns a line fills,
///   in order (a setting kept per layer gives a column per element,
///   `scroll[0]`);
/// - `push_row(&self, values: &mut Vec<u32>)`: push the line's values, one
///   per column.
///
/// ```
/// pub struct Chip {
///     blank: bool,
///     scroll: [u16; 2],
/// }
///
/// retro_core::line_state! {
///     /// The settings a line was drawn with.
///     #[derive(Debug, Clone, Copy, PartialEq, Eq)]
///     pub struct LineState for Chip {
///         /// The screen was blanked.
///         blank: bool = blank,
///         /// The scroll of each layer.
///         scroll: [u16; 2] = scroll,
///     }
/// }
///
/// let chip = Chip { blank: true, scroll: [3, 4] };
/// let line = LineState::capture(&chip);
/// assert_eq!(LineState::column_names(), ["blank", "scroll[0]", "scroll[1]"]);
/// let mut values = Vec::new();
/// line.push_row(&mut values);
/// assert_eq!(values, [1, 3, 4]);
///
/// let mut other = Chip { blank: false, scroll: [0, 0] };
/// line.apply(&mut other);
/// assert_eq!((other.blank, other.scroll), (true, [3, 4]));
/// ```
#[macro_export]
macro_rules! line_state {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident for $chip:ty {
            $( $(#[$doc:meta])* $setting:ident : $ty:ty = $field:ident ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        $vis struct $name {
            $( $(#[$doc])* pub $setting: $ty, )*
        }

        impl $name {
            /// The settings as the chip holds them now.
            pub(crate) fn capture(chip: &$chip) -> Self {
                Self { $( $setting: chip.$field, )* }
            }

            /// Put these settings into the chip, as they were when they were
            /// taken.
            pub(crate) fn apply(&self, chip: &mut $chip) {
                $( chip.$field = self.$setting; )*
            }

            /// The names of the columns a line's settings fill in a table,
            /// in order: a setting kept per layer gives a column per element
            /// (`name[0]`).
            pub fn column_names() -> ::std::vec::Vec<::std::string::String> {
                let mut names = ::std::vec::Vec::new();
                $(
                    <$ty as $crate::capture::TableColumns>::push_names(
                        ::core::stringify!($setting),
                        &mut names,
                    );
                )*
                names
            }

            /// Push this line's values, one per column of
            /// [`Self::column_names`].
            pub fn push_row(&self, values: &mut ::std::vec::Vec<u32>) {
                $( $crate::capture::TableColumns::push_values(&self.$setting, values); )*
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_distinct_palette_is_kept_once_in_the_order_first_used() {
        let red = [1u16, 2, 3, 4];
        let blue = [5u16, 6, 7, 8];
        let palettes = distinct_palettes([Some(red), Some(red), None, Some(blue), Some(red)]);
        assert_eq!(palettes.colours_per_palette, 4);
        assert_eq!(palettes.palettes, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(palettes.palette_of_line, [0, 0, NO_PALETTE, 1, 0]);
        assert_eq!(palettes.changes, None);
    }

    #[test]
    fn no_line_gives_no_palette() {
        let palettes = distinct_palettes::<256>(std::iter::empty());
        assert_eq!(palettes.colours_per_palette, 256);
        assert!(palettes.palettes.is_empty() && palettes.palette_of_line.is_empty());
        let unrecorded = distinct_palettes::<64>([None, None]);
        assert!(unrecorded.palettes.is_empty());
        assert_eq!(unrecorded.palette_of_line, [NO_PALETTE, NO_PALETTE]);
    }

    #[test]
    fn a_palette_moved_line_by_line_is_indexed_as_it_comes() {
        // A palette changed as the lines are drawn, as a core replays the
        // writes made between them: a colour written on line 2 and put back
        // on line 4.
        let mut colours = [0u16; 64];
        let palettes = distinct_palettes((0..6).map(|line| {
            colours[5] = if (2..4).contains(&line) { 0x0EEE } else { 0 };
            Some(colours)
        }));
        assert_eq!(palettes.palette_of_line, [0, 0, 1, 1, 0, 0]);
        assert_eq!(palettes.palettes.len(), 2 * 64);
        assert_eq!(palettes.palettes[64 + 5], 0x0EEE);
    }

    /// A part of a setting broken into flags, as a core's window selection.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    struct Window {
        on: bool,
        invert: bool,
    }

    impl TableColumns for Window {
        fn push_names(name: &str, names: &mut Vec<String>) {
            for part in ["on", "invert"] {
                names.push(format!("{name}.{part}"));
            }
        }
        fn push_values(&self, values: &mut Vec<u32>) {
            values.extend([u32::from(self.on), u32::from(self.invert)]);
        }
    }

    /// A small chip whose registers a line reads.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    struct Chip {
        blank: bool,
        mode: u8,
        scroll: [u16; 2],
        tile_base: u32,
        windows: [Window; 2],
        /// Not a line setting: left alone by `apply`.
        counter: u64,
    }

    crate::line_state! {
        /// The settings a test line was drawn with.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        struct TestLine for Chip {
            /// The screen was blanked.
            blanked: bool = blank,
            /// The mode.
            mode: u8 = mode,
            /// The scroll of each layer.
            scroll: [u16; 2] = scroll,
            /// Where the tiles are.
            tile_base: u32 = tile_base,
            /// The windows of each layer.
            windows: [Window; 2] = windows,
        }
    }

    fn busy_chip() -> Chip {
        Chip {
            blank: true,
            mode: 7,
            scroll: [0x123, 0x3FF],
            tile_base: 0x1_2000,
            windows: [
                Window {
                    on: true,
                    invert: false,
                },
                Window {
                    on: false,
                    invert: true,
                },
            ],
            counter: 99,
        }
    }

    #[test]
    fn a_line_s_settings_name_their_columns_in_order() {
        assert_eq!(
            TestLine::column_names(),
            [
                "blanked",
                "mode",
                "scroll[0]",
                "scroll[1]",
                "tile_base",
                "windows[0].on",
                "windows[0].invert",
                "windows[1].on",
                "windows[1].invert",
            ]
        );
    }

    #[test]
    fn a_line_s_settings_push_one_value_a_column() {
        let line = TestLine::capture(&busy_chip());
        let mut values = vec![42];
        line.push_row(&mut values);
        assert_eq!(values, [42, 1, 7, 0x123, 0x3FF, 0x1_2000, 1, 0, 0, 1]);
        assert_eq!(values.len() - 1, TestLine::column_names().len());
    }

    #[test]
    fn apply_after_capture_puts_back_every_setting_and_nothing_else() {
        let source = busy_chip();
        let line = TestLine::capture(&source);
        assert_eq!(
            line,
            TestLine {
                blanked: true,
                mode: 7,
                scroll: [0x123, 0x3FF],
                tile_base: 0x1_2000,
                windows: source.windows,
            }
        );
        let mut target = Chip {
            counter: 5,
            ..Chip::default()
        };
        line.apply(&mut target);
        assert_eq!(
            target,
            Chip {
                counter: 5,
                ..source
            }
        );
    }
}
