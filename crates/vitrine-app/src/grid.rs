//! Mapping between grid cells (layout.json) and physical pixels on a monitor.

use vitrine_desktop::monitors::Monitor;
use vitrine_widgets::layout::GridConfig;
use windows::Win32::Foundation::RECT;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    /// Top-left of cell (0, 0), physical pixels.
    pub origin: (f32, f32),
    pub cell: f32,
    pub gap: f32,
    /// DIP → pixel factor.
    pub scale: f32,
    pub cols: i32,
    pub rows: i32,
}

impl Geometry {
    pub fn new(grid: &GridConfig, work: RECT, scale: f32) -> Geometry {
        let fit = |px: i32| {
            let dips = px as f32 / scale - 2.0 * grid.margin;
            (((dips + grid.gap) / (grid.cell + grid.gap)).floor() as i32).max(1)
        };
        Geometry {
            origin: (
                work.left as f32 + grid.margin * scale,
                work.top as f32 + grid.margin * scale,
            ),
            cell: grid.cell,
            gap: grid.gap,
            scale,
            cols: fit(work.right - work.left),
            rows: fit(work.bottom - work.top),
        }
    }

    pub fn for_monitor(grid: &GridConfig, m: &Monitor) -> Geometry {
        Geometry::new(grid, m.work, m.scale)
    }

    fn pitch(&self) -> f32 {
        (self.cell + self.gap) * self.scale
    }

    /// Size in DIPs of `w` × `h` cells.
    pub fn dips(&self, w: i32, h: i32) -> (f32, f32) {
        let f = |n: i32| n as f32 * self.cell + (n - 1).max(0) as f32 * self.gap;
        (f(w), f(h))
    }

    /// Pixel rectangle of a cell span.
    pub fn rect(&self, x: i32, y: i32, w: i32, h: i32) -> RECT {
        let (dw, dh) = self.dips(w, h);
        let left = (self.origin.0 + x as f32 * self.pitch()).round() as i32;
        let top = (self.origin.1 + y as f32 * self.pitch()).round() as i32;
        RECT {
            left,
            top,
            right: left + (dw * self.scale).round() as i32,
            bottom: top + (dh * self.scale).round() as i32,
        }
    }

    /// Nearest cell to a pixel position (for snapping a dragged top-left corner).
    pub fn snap(&self, px: f32, py: f32) -> (i32, i32) {
        (
            ((px - self.origin.0) / self.pitch()).round() as i32,
            ((py - self.origin.1) / self.pitch()).round() as i32,
        )
    }

    /// Number of cells spanned by a pixel length, at least 1.
    pub fn span(&self, px: f32) -> i32 {
        (((px / self.scale) + self.gap) / (self.cell + self.gap))
            .round()
            .max(1.0) as i32
    }

    /// Clamps a cell span so it stays on the grid.
    pub fn clamp(&self, x: i32, y: i32, w: i32, h: i32) -> (i32, i32, i32, i32) {
        let w = w.clamp(1, self.cols);
        let h = h.clamp(1, self.rows);
        (x.clamp(0, self.cols - w), y.clamp(0, self.rows - h), w, h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geo(scale: f32) -> Geometry {
        let work = RECT {
            left: 0,
            top: 0,
            right: (1920.0 * scale) as i32,
            bottom: (1032.0 * scale) as i32,
        };
        Geometry::new(&GridConfig::default(), work, scale)
    }

    #[test]
    fn fits_cells_in_work_area() {
        let g = geo(1.0);
        // (1920 - 48 + 8) / 72 = 26.1
        assert_eq!(g.cols, 26);
        assert_eq!(g.rows, 13);
        assert_eq!(geo(1.5).cols, 26);
    }

    #[test]
    fn rect_and_snap_agree() {
        let g = geo(1.5);
        let r = g.rect(3, 2, 4, 2);
        assert_eq!(r.left, (36.0 + 3.0 * 108.0) as i32);
        assert_eq!(r.right - r.left, ((4.0 * 64.0 + 3.0 * 8.0) * 1.5) as i32);
        assert_eq!(g.snap(r.left as f32 + 10.0, r.top as f32 - 10.0), (3, 2));
        assert_eq!(g.span((r.right - r.left) as f32), 4);
        assert_eq!(g.clamp(30, -1, 4, 2), (22, 0, 4, 2));
    }
}
