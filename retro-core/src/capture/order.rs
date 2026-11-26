//! The order a console puts its layers in, a line at a time: each place of
//! it the pixels of one priority of one layer ([`DrawSlot`]), and the runs
//! of a frame's lines that put them in one order ([`DrawOrder`]).

/// A place in the order a console puts a line's layers in: the pixels of
/// layer `layer` whose priority is `priority`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DrawSlot {
    /// The layer, an id of the core's layer list (the ids
    /// [`super::CaptureCore::render_layer`] takes), the sprites' included.
    pub layer: u32,
    /// The priority the layer's pixels at this place have, as the hardware
    /// gives a pixel its priority (a cell's priority bit, a sprite's
    /// priority; documented by the core); 0 for a layer whose pixels have
    /// none.
    pub priority: u8,
}

impl DrawSlot {
    /// The pixels of layer `layer` of priority `priority`.
    pub const fn new(layer: u32, priority: u8) -> Self {
        Self { layer, priority }
    }
}

/// The order a run of a frame's lines put their layers in: the `lines`
/// lines from `first_line` on, counted as [`crate::PictureGrid::first_line`]
/// counts them, each put its layers in the order of `slots`.
///
/// `slots` runs from front to back: where two places both have a pixel,
/// the console puts the earlier one's in front. The order is what the
/// layers' priorities decide; what the console then shows of each layer
/// (its screens and windows, a colour effect, a layer hidden) is decided
/// afterwards, and a line drawn blank shows none of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrawOrder {
    /// The run's first line.
    pub first_line: u32,
    /// The lines in the run, one after another.
    pub lines: u32,
    /// The places, front to back.
    pub slots: Vec<DrawSlot>,
}

/// The runs of `lines`, each line's number and the order it put its layers
/// in, front to back, given in the order the lines were drawn: a line that
/// follows the line before it and has the same order joins its run.
pub fn draw_order_runs<'a>(
    lines: impl IntoIterator<Item = (u32, &'a [DrawSlot])>,
) -> Vec<DrawOrder> {
    let mut runs: Vec<DrawOrder> = Vec::new();
    for (line, slots) in lines {
        if let Some(run) = runs.last_mut() {
            let follows = run.first_line.checked_add(run.lines) == Some(line);
            if follows && run.slots == slots {
                run.lines += 1;
                continue;
            }
        }
        runs.push(DrawOrder {
            first_line: line,
            lines: 1,
            slots: slots.to_vec(),
        });
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: [DrawSlot; 2] = [DrawSlot::new(3, 1), DrawSlot::new(0, 0)];
    const OTHER: [DrawSlot; 1] = [DrawSlot::new(0, 0)];

    #[test]
    fn lines_in_one_order_make_one_run() {
        let runs = draw_order_runs((0..224).map(|line| (line, &ONE[..])));
        assert_eq!(
            runs,
            [DrawOrder {
                first_line: 0,
                lines: 224,
                slots: ONE.to_vec()
            }]
        );
    }

    #[test]
    fn a_change_of_order_starts_a_run_and_so_does_a_line_left_out() {
        let lines = [
            (0, &ONE[..]),
            (1, &ONE[..]),
            (2, &OTHER[..]),
            (3, &OTHER[..]),
            (5, &OTHER[..]),
            (6, &ONE[..]),
        ];
        let runs = draw_order_runs(lines);
        let shape: Vec<(u32, u32, usize)> = runs
            .iter()
            .map(|run| (run.first_line, run.lines, run.slots.len()))
            .collect();
        assert_eq!(shape, [(0, 2, 2), (2, 2, 1), (5, 1, 1), (6, 1, 2)]);
    }

    #[test]
    fn no_line_makes_no_run() {
        assert!(draw_order_runs(std::iter::empty()).is_empty());
    }
}
