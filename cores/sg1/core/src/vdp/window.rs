//! The part of the screen the window plane takes from plane A.
//!
//! Registers 17 and 18 each place one edge. Register 18 picks whole lines:
//! with its bit 7 clear the window takes the lines above its edge, with the
//! bit set the lines from the edge down. On every other line, register 17
//! picks columns the same way: bit 7 clear, the columns left of its edge;
//! set, the columns from the edge rightwards. So an edge at 0 gives the
//! window nothing when it grows from the top or the left, and everything
//! when it grows from the bottom or the right, and an edge past the screen
//! does the opposite. Checked against Genesis Plus GX (`window_clip()`
//! swaps plane A and the window on bit 7 for every edge, 0 included) and
//! MAME. Rings of Power draws its map and its dialogue box this way, with
//! register 17 at `$80` and register 18 at `$00`.

/// Where the window plane replaces plane A, decoded from registers 17 and 18.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowArea {
    /// The column edge in pixels (register 17 bits 4-0, in units of two
    /// cells, 16 pixels).
    pub column_edge: u16,
    /// The window takes the columns from the edge rightwards (register 17
    /// bit 7); otherwise the columns left of it.
    pub from_right: bool,
    /// The line edge (register 18 bits 4-0, in units of one cell, 8 lines).
    pub line_edge: u16,
    /// The window takes the lines from the edge down (register 18 bit 7);
    /// otherwise the lines above it.
    pub from_bottom: bool,
}

impl WindowArea {
    /// Decode registers 17 (columns) and 18 (lines).
    pub fn from_registers(reg17: u8, reg18: u8) -> Self {
        Self {
            column_edge: u16::from(reg17 & 0x1F) * 16,
            from_right: (reg17 & 0x80) != 0,
            line_edge: u16::from(reg18 & 0x1F) * 8,
            from_bottom: (reg18 & 0x80) != 0,
        }
    }

    /// Whether the window takes the whole of line `y`.
    pub fn takes_line(&self, y: u16) -> bool {
        self.from_bottom == (y >= self.line_edge)
    }

    /// Whether the window takes column `x` on a line it does not take whole.
    pub fn takes_column(&self, x: u16) -> bool {
        self.from_right == (x >= self.column_edge)
    }

    /// Whether the window covers the pixel at (`x`, `y`).
    pub fn covers(&self, x: u16, y: u16) -> bool {
        self.takes_line(y) || self.takes_column(x)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(reg17: u8, reg18: u8) -> WindowArea {
        WindowArea::from_registers(reg17, reg18)
    }

    #[test]
    fn edges_at_zero_growing_from_the_top_and_left_give_nothing() {
        let a = area(0x00, 0x00);
        assert!(!a.covers(0, 0));
        assert!(!a.covers(160, 112));
        assert!(!a.covers(319, 223));
    }

    #[test]
    fn a_column_edge_at_zero_growing_from_the_right_takes_every_line() {
        // Rings of Power's map and dialogue box: R17=$80, R18=$00.
        let a = area(0x80, 0x00);
        assert!(a.covers(0, 0));
        assert!(a.covers(160, 112));
        assert!(a.covers(319, 223));
    }

    #[test]
    fn a_line_edge_at_zero_growing_from_the_bottom_takes_every_line() {
        let a = area(0x00, 0x80);
        assert!(a.covers(0, 0));
        assert!(a.covers(319, 223));
    }

    #[test]
    fn columns_split_on_their_edge() {
        let right = area(0x8A, 0x00); // from pixel 160 rightwards
        assert!(!right.covers(159, 100));
        assert!(right.covers(160, 100));
        let left = area(0x0A, 0x00); // pixels 0-159
        assert!(left.covers(159, 100));
        assert!(!left.covers(160, 100));
    }

    #[test]
    fn lines_split_on_their_edge() {
        let down = area(0x00, 0x85); // from line 40 down
        assert!(!down.covers(0, 39));
        assert!(down.covers(0, 40));
        let top = area(0x00, 0x05); // lines 0-39
        assert!(top.covers(0, 39));
        assert!(!top.covers(0, 40));
    }

    #[test]
    fn the_window_is_the_union_of_its_lines_and_its_columns() {
        // Lines from 200 down, and columns 0-63 on the lines above.
        let a = area(0x04, 0x99);
        assert!(a.covers(300, 210), "a whole line below the line edge");
        assert!(a.covers(10, 50), "a column left of the column edge");
        assert!(!a.covers(100, 50), "neither");
    }

    #[test]
    fn an_edge_past_the_screen_gives_all_from_the_left_and_none_from_the_right() {
        // 31 two-cell columns = pixel 496, past both display widths.
        assert!(area(0x1F, 0x00).covers(319, 0));
        assert!(!area(0x9F, 0x00).covers(319, 0));
        // 31 cells = line 248, past both display heights.
        assert!(area(0x00, 0x1F).covers(0, 239));
        assert!(!area(0x00, 0x9F).covers(0, 239));
    }
}
