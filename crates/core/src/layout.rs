//! Where multiview's tiles sit on screen. Pure maths, shared by the reducer (each tile's quality
//! comes from its size, see `quality`) and the view-model (where each tile is drawn).
//!
//! Everything is in the 1280x800 layout's logical pixels. The window scales that layout to fit
//! the screen (`fit_scale`), so a tile's real size is its layout size times that scale.

/// The size the layout is designed for, in logical px (`Theme` in `theme.slint`).
pub const DESIGN_WIDTH: f32 = 1280.0;
pub const DESIGN_HEIGHT: f32 = 800.0;

/// The space for tiles: between the multiview header (72 px) and the hints bar (72 px, plus 24 px
/// of air above it), with the screens' usual 64 px side margins.
pub const AREA: Rect = Rect {
    x: 64.0,
    y: 72.0,
    w: 1152.0,
    h: 632.0,
};

/// The space between two tiles.
pub const GAP: f32 = 16.0;

/// The whole screen: a stream in full screen.
pub const FULL_SCREEN: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: DESIGN_WIDTH,
    h: DESIGN_HEIGHT,
};

/// The most streams multiview plays at once.
pub const MAX_STREAMS: usize = 9;

/// Width over height of a video tile.
const ASPECT: f32 = 16.0 / 9.0;

/// The scale factor that makes the 1280x800 layout fill a window of `width` x `height` physical
/// pixels with nothing cut off: 1.0 on a Steam Deck, 1.35 on a 1080p TV, 2.7 on a 4K one. A window
/// of another shape gets extra room on one axis. `None` for an empty (minimised) window.
pub fn fit_scale(width: u32, height: u32) -> Option<f32> {
    if width == 0 || height == 0 {
        return None;
    }
    Some((width as f32 / DESIGN_WIDTH).min(height as f32 / DESIGN_HEIGHT))
}

/// How many physical pixels tall something `layout_height` tall in the layout is on a display of
/// `display` (width, height) physical pixels. A display not known yet counts as the Deck's
/// 1280x800, which errs towards lower quality.
pub fn pixel_height(layout_height: f32, display: (u32, u32)) -> f32 {
    layout_height * fit_scale(display.0, display.1).unwrap_or(1.0)
}

/// A rectangle in the layout's logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    /// The two share some area (touching edges do not count).
    pub fn overlaps(&self, other: &Rect) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }

    /// `other` lies entirely inside this one. A hair of float error is allowed.
    pub fn contains(&self, other: &Rect) -> bool {
        const E: f32 = 0.01;
        other.x >= self.x - E
            && other.y >= self.y - E
            && other.right() <= self.right() + E
            && other.bottom() <= self.bottom() + E
    }

    /// The point in the middle.
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

/// How multiview arranges its tiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// Equal tiles with this many places, one of `Layout::GRID_SIZES`. Places without a stream are
    /// *Add a stream* slots.
    Grid(u8),
    /// The stream with sound is big and the others sit around it: down one side up to 4 streams,
    /// on three sides of it from 5 to 9.
    Spotlight,
}

impl Layout {
    /// The grid sizes, smallest first.
    pub const GRID_SIZES: [u8; 5] = [2, 3, 4, 6, 9];

    /// Every layout in the order LB/RB step through them.
    pub const ALL: [Layout; 6] = [
        Layout::Grid(2),
        Layout::Grid(3),
        Layout::Grid(4),
        Layout::Grid(6),
        Layout::Grid(9),
        Layout::Spotlight,
    ];

    /// How many places the layout has for `streams` streams. A grid always has its own size (a
    /// size that is not one of `GRID_SIZES` counts as the next one up). Spotlight has 4 places up
    /// to 4 streams, and 9 above that.
    pub fn places(self, streams: usize) -> usize {
        match self {
            Layout::Grid(n) => grid_size(usize::from(n)),
            Layout::Spotlight if streams <= 4 => 4,
            Layout::Spotlight => MAX_STREAMS,
        }
    }

    /// The layout has a place for each of `streams` streams.
    pub fn fits(self, streams: usize) -> bool {
        streams <= self.places(streams)
    }

    /// The smallest grid with a place for each of `streams` streams (the grid of 9 for more).
    pub fn smallest_grid(streams: usize) -> Layout {
        Layout::Grid(grid_size(streams) as u8)
    }

    /// The next (or, with `forward == false`, the previous) layout that fits `streams` streams,
    /// wrapping around. Grids that are too small are skipped; Spotlight always fits.
    pub fn cycled(self, streams: usize, forward: bool) -> Layout {
        let fitting: Vec<Layout> = Self::ALL.into_iter().filter(|l| l.fits(streams)).collect();
        let len = fitting.len() as isize;
        let at = fitting.iter().position(|l| *l == self);
        let next = match (at, forward) {
            (Some(at), true) => at as isize + 1,
            (Some(at), false) => at as isize - 1,
            // From a layout that no longer fits, start at the first (or last) that does.
            (None, true) => 0,
            (None, false) => len - 1,
        };
        fitting[next.rem_euclid(len) as usize]
    }
}

/// The smallest of `Layout::GRID_SIZES` that is at least `n`, or the largest.
fn grid_size(n: usize) -> usize {
    Layout::GRID_SIZES
        .into_iter()
        .map(usize::from)
        .find(|&size| size >= n)
        .unwrap_or(MAX_STREAMS)
}

/// Where each place of `layout` sits when it holds `streams` streams, in place order. There are
/// `layout.places(streams)` of them. In Spotlight, place `sound` (the stream with sound) is the
/// big one and the other places go around it in order; grids ignore `sound`.
pub fn rects(layout: Layout, streams: usize, sound: usize) -> Vec<Rect> {
    let places = layout.places(streams);
    match layout {
        Layout::Grid(_) => grid(places),
        Layout::Spotlight => spotlight(places, sound.min(places - 1)),
    }
}

/// Columns and rows of a grid with `places` places.
fn grid_shape(places: usize) -> (usize, usize) {
    match places {
        0..=2 => (2, 1),
        3..=4 => (2, 2),
        5..=6 => (3, 2),
        _ => (3, 3),
    }
}

/// Equal 16:9 tiles, as big as the area allows, centred in it. The third tile of a grid of 3 sits
/// in the middle of the second row.
fn grid(places: usize) -> Vec<Rect> {
    let (cols, rows) = grid_shape(places);
    let (cols_f, rows_f) = (cols as f32, rows as f32);
    let mut h = (AREA.h - (rows_f - 1.0) * GAP) / rows_f;
    let mut w = h * ASPECT;
    if cols_f * w + (cols_f - 1.0) * GAP > AREA.w {
        w = (AREA.w - (cols_f - 1.0) * GAP) / cols_f;
        h = w / ASPECT;
    }
    let total_w = cols_f * w + (cols_f - 1.0) * GAP;
    let total_h = rows_f * h + (rows_f - 1.0) * GAP;
    let x0 = AREA.x + (AREA.w - total_w) / 2.0;
    let y0 = AREA.y + (AREA.h - total_h) / 2.0;
    (0..places)
        .map(|i| {
            let (col, row) = ((i % cols) as f32, (i / cols) as f32);
            let x = if places == 3 && i == 2 {
                AREA.x + (AREA.w - w) / 2.0
            } else {
                x0 + col * (w + GAP)
            };
            Rect {
                x,
                y: y0 + row * (h + GAP),
                w,
                h,
            }
        })
        .collect()
}

/// The small tiles of Spotlight with up to 4 streams.
const SPOTLIGHT_SMALL: (f32, f32) = (320.0, 180.0);

/// Up to 4 places: the big tile on the left, the small ones in a column on its right, both centred
/// in height. From 5: a 4 x 3 board of small cells, the big tile on the middle 2 x 2 of the top two
/// rows and the other eight places around it.
fn spotlight(places: usize, big: usize) -> Vec<Rect> {
    let mut out = Vec::with_capacity(places);
    if places <= 4 {
        let (sw, sh) = SPOTLIGHT_SMALL;
        let bw = AREA.w - GAP - sw;
        let bh = bw / ASPECT;
        let big_rect = Rect {
            x: AREA.x,
            y: AREA.y + (AREA.h - bh) / 2.0,
            w: bw,
            h: bh,
        };
        let smalls = (places - 1) as f32;
        let column_h = smalls * sh + (smalls - 1.0) * GAP;
        let top = AREA.y + (AREA.h - column_h) / 2.0;
        let mut next = 0.0;
        for i in 0..places {
            if i == big {
                out.push(big_rect);
            } else {
                out.push(Rect {
                    x: AREA.x + bw + GAP,
                    y: top + next * (sh + GAP),
                    w: sw,
                    h: sh,
                });
                next += 1.0;
            }
        }
        return out;
    }
    let w = (AREA.w - 3.0 * GAP) / 4.0;
    let h = w / ASPECT;
    let y0 = AREA.y + (AREA.h - (3.0 * h + 2.0 * GAP)) / 2.0;
    let cell = |col: f32, row: f32| Rect {
        x: AREA.x + col * (w + GAP),
        y: y0 + row * (h + GAP),
        w,
        h,
    };
    const AROUND: [(f32, f32); 8] = [
        (0.0, 0.0),
        (3.0, 0.0),
        (0.0, 1.0),
        (3.0, 1.0),
        (0.0, 2.0),
        (1.0, 2.0),
        (2.0, 2.0),
        (3.0, 2.0),
    ];
    let mut around = AROUND.iter();
    for i in 0..places {
        if i == big {
            // Two cells and a gap each way is a little wider than 16:9, so the big tile takes
            // the block's full width and sits in the middle of its height.
            let at = cell(1.0, 0.0);
            let (bw, block_h) = (2.0 * w + GAP, 2.0 * h + GAP);
            let bh = bw / ASPECT;
            out.push(Rect {
                x: at.x,
                y: at.y + (block_h - bh) / 2.0,
                w: bw,
                h: bh,
            });
        } else if let Some(&(col, row)) = around.next() {
            out.push(cell(col, row));
        }
    }
    out
}
