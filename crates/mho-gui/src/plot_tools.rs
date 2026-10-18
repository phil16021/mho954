//! Interaction state of the scope plot: time and level cursors (draggable), pending view
//! changes (reset, fit, zoom), the region analysed by cursor measurements and the FFT window.
//! The pure helpers here are what the plot code in `app.rs` calls; they are unit-tested.

use mho_driver::{Preamble, Waveform};

/// A cursor line that can be dragged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    A,
    B,
    Y1,
    Y2,
}

/// View change applied on the next frame (inside the plot, where the bounds live).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ViewAction {
    /// Whole record horizontally, default vertical range.
    Reset,
    /// Vertical range fitted to the visible data.
    FitY,
    /// Zoom by these factors (> 1 zooms in) around `center` (plot coordinates).
    Zoom {
        x: f64,
        y: f64,
        center: Option<[f64; 2]>,
    },
    /// Show this time range.
    ShowX(f64, f64),
}

#[derive(Debug, Default)]
pub struct PlotTools {
    /// Time cursors (s).
    pub a: Option<f64>,
    pub b: Option<f64>,
    /// Level cursors in plot units (divisions or volts, like the y axis).
    pub y1: Option<f64>,
    pub y2: Option<f64>,
    /// Show measurements of the region between A and B.
    pub measure_gate: bool,
    /// Time range the FFT is computed over (`None`: whole record).
    pub fft_gate: Option<(f64, f64)>,
    pub pending: Option<ViewAction>,
    /// Cursor being dragged.
    pub drag: Option<Handle>,
    /// Plot position of the last right click (where the context menu acts).
    pub menu_at: Option<[f64; 2]>,
    /// Visible time range of the last frame.
    pub view_x: Option<(f64, f64)>,
}

impl PlotTools {
    pub fn clear_cursors(&mut self) {
        self.a = None;
        self.b = None;
        self.y1 = None;
        self.y2 = None;
        self.measure_gate = false;
    }

    /// A..B in increasing order, if both are set and differ.
    pub fn gate(&self) -> Option<(f64, f64)> {
        let (a, b) = (self.a?, self.b?);
        (a != b).then(|| (a.min(b), a.max(b)))
    }

    pub fn set(&mut self, h: Handle, x: f64, y: f64) {
        match h {
            Handle::A => self.a = Some(x),
            Handle::B => self.b = Some(x),
            Handle::Y1 => self.y1 = Some(y),
            Handle::Y2 => self.y2 = Some(y),
        }
    }

    /// The cursor within `tolerance` screen pixels of `pointer`, given a function mapping
    /// plot coordinates to screen pixels.
    pub fn handle_near(
        &self,
        pointer: [f32; 2],
        to_screen: impl Fn(f64, f64) -> [f32; 2],
        tolerance: f32,
    ) -> Option<Handle> {
        let mut best: Option<(f32, Handle)> = None;
        let mut consider = |d: f32, h: Handle| {
            if d <= tolerance && best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, h));
            }
        };
        for (x, h) in [(self.a, Handle::A), (self.b, Handle::B)] {
            if let Some(x) = x {
                consider((to_screen(x, 0.0)[0] - pointer[0]).abs(), h);
            }
        }
        for (y, h) in [(self.y1, Handle::Y1), (self.y2, Handle::Y2)] {
            if let Some(y) = y {
                consider((to_screen(0.0, y)[1] - pointer[1]).abs(), h);
            }
        }
        best.map(|(_, h)| h)
    }
}

/// Sample indices of `p` whose time lies in `t0..=t1` (clamped to `len` samples).
pub fn index_range(p: &Preamble, len: usize, t0: f64, t1: f64) -> Option<std::ops::Range<usize>> {
    if len == 0 || p.x_increment <= 0.0 {
        return None;
    }
    let idx = |t: f64| (t - p.x_origin) / p.x_increment + p.x_reference;
    // Tolerance against rounding: a cursor placed on a sample must include it.
    let lo = (idx(t0.min(t1)) - 1e-6).ceil().max(0.0);
    let hi = ((idx(t0.max(t1)) + 1e-6).floor() + 1.0).min(len as f64);
    (hi > lo).then_some(lo as usize..hi as usize)
}

/// Volts of `w` between `t0` and `t1` (at least two samples).
pub fn gated_volts(w: &Waveform, t0: f64, t1: f64) -> Option<Vec<f64>> {
    let r = index_range(&w.preamble, w.raw.len(), t0, t1)?;
    (r.len() >= 2).then(|| w.raw[r].iter().map(|&c| w.preamble.volts(c)).collect())
}

/// `lo..hi` zoomed by `factor` (> 1 zooms in) around `center`.
pub fn zoom_range(lo: f64, hi: f64, factor: f64, center: f64) -> (f64, f64) {
    let f = factor.max(1e-6);
    (center - (center - lo) / f, center + (hi - center) / f)
}

/// Timebase `(scale per division, offset)` that shows `x0..x1` on the scope's 10 divisions.
pub fn timebase_for_view(x0: f64, x1: f64) -> (f64, f64) {
    ((x1 - x0).abs() / 10.0, (x0 + x1) / 2.0)
}

/// Volts of a channel at `div` divisions on screen (`div = (v + offset) / scale`).
pub fn div_to_volts(div: f64, scale: f64, offset: f64) -> f64 {
    div * scale - offset
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pre(points: usize, dt: f64, t0: f64) -> Preamble {
        Preamble {
            format: 1,
            kind: 0,
            points,
            count: 1,
            x_increment: dt,
            x_origin: t0,
            x_reference: 0.0,
            y_increment: 1.0,
            y_origin: 0.0,
            y_reference: 0.0,
        }
    }

    #[test]
    fn index_ranges() {
        let p = pre(1000, 1e-6, -500e-6);
        assert_eq!(index_range(&p, 1000, -500e-6, 499e-6), Some(0..1000));
        assert_eq!(index_range(&p, 1000, 0.0, 10e-6), Some(500..511));
        // reversed and clamped
        assert_eq!(index_range(&p, 1000, 1.0, 400e-6), Some(900..1000));
        assert_eq!(index_range(&p, 1000, 1.0, 2.0), None);
        assert_eq!(index_range(&p, 0, 0.0, 1.0), None);
    }

    #[test]
    fn gated_samples() {
        let w = Waveform {
            source: "CHAN1".into(),
            preamble: pre(10, 1.0, 0.0),
            raw: (0..10).collect(),
        };
        assert_eq!(gated_volts(&w, 2.0, 4.0).unwrap(), vec![2.0, 3.0, 4.0]);
        assert!(
            gated_volts(&w, 2.2, 2.8).is_none(),
            "fewer than two samples"
        );
    }

    #[test]
    fn view_math() {
        assert_eq!(zoom_range(0.0, 10.0, 2.0, 5.0), (2.5, 7.5));
        assert_eq!(zoom_range(0.0, 10.0, 0.5, 0.0), (0.0, 20.0));
        assert_eq!(timebase_for_view(-1e-3, 1e-3), (2e-4, 0.0));
        assert!((div_to_volts(2.0, 0.5, 0.1) - 0.9).abs() < 1e-12);
    }

    #[test]
    fn cursor_handles() {
        let t = PlotTools {
            a: Some(1.0),
            b: Some(5.0),
            y1: Some(2.0),
            ..Default::default()
        };
        // 10 px per unit in x, y flipped like a screen.
        let to_screen = |x: f64, y: f64| [(x * 10.0) as f32, (100.0 - y * 10.0) as f32];
        assert_eq!(t.handle_near([12.0, 50.0], to_screen, 6.0), Some(Handle::A));
        assert_eq!(t.handle_near([49.0, 50.0], to_screen, 6.0), Some(Handle::B));
        assert_eq!(
            t.handle_near([30.0, 79.0], to_screen, 6.0),
            Some(Handle::Y1)
        );
        assert_eq!(t.handle_near([30.0, 50.0], to_screen, 6.0), None);
        assert_eq!(t.gate(), Some((1.0, 5.0)));
    }
}
