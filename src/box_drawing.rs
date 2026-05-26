//! Procedural rendering of box-drawing, block-element, and braille glyphs.
//!
//! Most monospace fonts draw box-drawing chars (U+2500–U+259F) slightly
//! narrower than the cell, so adjacent `─` cells show a visible gap. The
//! braille range (U+2800–U+28FF), used by btop/htop for bar graphs, has the
//! same problem.
//!
//! We sidestep the font for these ranges and emit quads sized to the cell.
//! Returns `true` if the char was handled (the caller should suppress the
//! font glyph for that cell).

use crate::quad::Quad;

pub fn is_handled(ch: char) -> bool {
    let c = ch as u32;
    (0x2500..=0x259F).contains(&c) || (0x2800..=0x28FF).contains(&c)
}

/// Append quads for `ch` rendered inside the cell at (`x`, `y`) with size
/// (`cell_w`, `cell_h`) in physical pixels. `color` is the linear-space RGBA
/// foreground.
#[allow(clippy::too_many_arguments)]
pub fn push_quads(
    quads: &mut Vec<Quad>,
    ch: char,
    x: f32,
    y: f32,
    cell_w: f32,
    cell_h: f32,
    color: [f32; 4],
    scale: f32,
) -> bool {
    let c = ch as u32;
    if (0x2500..=0x257F).contains(&c) {
        push_box_drawing(quads, ch, x, y, cell_w, cell_h, color, scale);
        true
    } else if (0x2580..=0x259F).contains(&c) {
        push_block_element(quads, ch, x, y, cell_w, cell_h, color);
        true
    } else if (0x2800..=0x28FF).contains(&c) {
        push_braille(quads, ch, x, y, cell_w, cell_h, color, scale);
        true
    } else {
        false
    }
}

fn rect(quads: &mut Vec<Quad>, x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    quads.push(Quad {
        rect: [x, y, w, h],
        color,
    });
}

/// Line weight at a cell edge. None = no line, Light = 1× stroke, Heavy = 2×
/// stroke, Double = two parallel light strokes.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Weight {
    None,
    Light,
    Heavy,
    Double,
}

/// Dash style for a horizontal/vertical line.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Dash {
    Solid,
    Double, // U+254C/D, U+254E/F
    Triple, // U+2504..2507
    Quad,   // U+2508..250B
}

struct Sides {
    left: Weight,
    right: Weight,
    up: Weight,
    down: Weight,
    dash: Dash,
}

impl Sides {
    const fn none() -> Self {
        Self {
            left: Weight::None,
            right: Weight::None,
            up: Weight::None,
            down: Weight::None,
            dash: Dash::Solid,
        }
    }
}

fn weight_for(w: Weight, scale: f32) -> f32 {
    match w {
        Weight::None => 0.0,
        Weight::Light | Weight::Double => (scale).round().max(1.0),
        Weight::Heavy => (scale * 2.0).round().max(2.0),
    }
}

#[allow(clippy::too_many_arguments)]
fn push_box_drawing(
    quads: &mut Vec<Quad>,
    ch: char,
    x: f32,
    y: f32,
    cell_w: f32,
    cell_h: f32,
    color: [f32; 4],
    scale: f32,
) {
    let s = sides_for(ch);

    let cx = (x + cell_w * 0.5).round();
    let cy = (y + cell_h * 0.5).round();

    let lw_h = weight_for(
        if s.left != Weight::None {
            s.left
        } else {
            s.right
        },
        scale,
    );
    let lw_v = weight_for(if s.up != Weight::None { s.up } else { s.down }, scale);

    // Half-line segments. Each direction is drawn from the cell edge to the
    // centre with the requested weight. A 1-cell-wide overdraw at the centre
    // is fine (it's the line crossing).
    let half_h_w = lw_v.max(lw_h); // ensure horizontals overlap vertical stem
    let half_v_w = lw_h.max(lw_v);
    let _ = (half_h_w, half_v_w);

    let draw_segment =
        |quads: &mut Vec<Quad>, x0: f32, y0: f32, x1: f32, y1: f32, w: f32, dash: Dash| {
            if w <= 0.0 {
                return;
            }
            let horizontal = (y1 - y0).abs() < 0.5;
            let half = w * 0.5;
            if horizontal {
                let (xa, xb) = if x0 < x1 { (x0, x1) } else { (x1, x0) };
                push_dashed(quads, xa, y0 - half, xb - xa, w, true, dash, color);
            } else {
                let (ya, yb) = if y0 < y1 { (y0, y1) } else { (y1, y0) };
                push_dashed(quads, x0 - half, ya, w, yb - ya, false, dash, color);
            }
        };

    // Horizontals to the left/right of centre.
    if matches!(s.left, Weight::Light | Weight::Heavy) {
        draw_segment(
            quads,
            x,
            cy,
            cx + lw_v * 0.5,
            cy,
            weight_for(s.left, scale),
            s.dash,
        );
    }
    if matches!(s.right, Weight::Light | Weight::Heavy) {
        draw_segment(
            quads,
            cx - lw_v * 0.5,
            cy,
            x + cell_w,
            cy,
            weight_for(s.right, scale),
            s.dash,
        );
    }
    if matches!(s.up, Weight::Light | Weight::Heavy) {
        draw_segment(
            quads,
            cx,
            y,
            cx,
            cy + lw_h * 0.5,
            weight_for(s.up, scale),
            s.dash,
        );
    }
    if matches!(s.down, Weight::Light | Weight::Heavy) {
        draw_segment(
            quads,
            cx,
            cy - lw_h * 0.5,
            cx,
            y + cell_h,
            weight_for(s.down, scale),
            s.dash,
        );
    }

    // Double lines: two parallel light strokes, offset by ~1 light stroke
    // width on each side of centre.
    let light_w = (scale).round().max(1.0);
    let off = (light_w * 1.5).round().max(1.0);
    if s.left == Weight::Double {
        rect(
            quads,
            x,
            cy - off - light_w * 0.5,
            cx - x + light_w * 0.5,
            light_w,
            color,
        );
        rect(
            quads,
            x,
            cy + off - light_w * 0.5,
            cx - x + light_w * 0.5,
            light_w,
            color,
        );
    }
    if s.right == Weight::Double {
        rect(
            quads,
            cx - light_w * 0.5,
            cy - off - light_w * 0.5,
            (x + cell_w) - cx + light_w * 0.5,
            light_w,
            color,
        );
        rect(
            quads,
            cx - light_w * 0.5,
            cy + off - light_w * 0.5,
            (x + cell_w) - cx + light_w * 0.5,
            light_w,
            color,
        );
    }
    if s.up == Weight::Double {
        rect(
            quads,
            cx - off - light_w * 0.5,
            y,
            light_w,
            cy - y + light_w * 0.5,
            color,
        );
        rect(
            quads,
            cx + off - light_w * 0.5,
            y,
            light_w,
            cy - y + light_w * 0.5,
            color,
        );
    }
    if s.down == Weight::Double {
        rect(
            quads,
            cx - off - light_w * 0.5,
            cy - light_w * 0.5,
            light_w,
            (y + cell_h) - cy + light_w * 0.5,
            color,
        );
        rect(
            quads,
            cx + off - light_w * 0.5,
            cy - light_w * 0.5,
            light_w,
            (y + cell_h) - cy + light_w * 0.5,
            color,
        );
    }

    // Diagonals: U+2571 ╱, U+2572 ╲, U+2573 ╳ — approximated by stacked
    // thin rects along the slope. Coarse but gap-free.
    if matches!(ch, '╱' | '╲' | '╳') {
        let steps = (cell_h.max(cell_w)).round() as i32;
        let w = (scale).round().max(1.0);
        if ch == '╱' || ch == '╳' {
            for i in 0..steps {
                let t = i as f32 / steps as f32;
                let px = x + cell_w - t * cell_w - w * 0.5;
                let py = y + t * cell_h;
                rect(quads, px, py, w, (cell_h / steps as f32).ceil(), color);
            }
        }
        if ch == '╲' || ch == '╳' {
            for i in 0..steps {
                let t = i as f32 / steps as f32;
                let px = x + t * cell_w - w * 0.5;
                let py = y + t * cell_h;
                rect(quads, px, py, w, (cell_h / steps as f32).ceil(), color);
            }
        }
    }

    // Rounded corners U+256D..U+2570 — for now drawn as plain corners. The
    // visual is square-cornered but gap-free, which is the goal here.
    // (Arc approximation could be added later.)
}

#[allow(clippy::too_many_arguments)]
fn push_dashed(
    quads: &mut Vec<Quad>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    horizontal: bool,
    dash: Dash,
    color: [f32; 4],
) {
    let n = match dash {
        Dash::Solid => {
            rect(quads, x, y, w, h, color);
            return;
        }
        Dash::Double => 2,
        Dash::Triple => 3,
        Dash::Quad => 4,
    };
    // Dashed: n segments separated by (n-1) gaps; total gap budget is ~25%.
    let span = if horizontal { w } else { h };
    let gap_total = (span * 0.25).max(n as f32);
    let dash_total = span - gap_total;
    let dash_len = dash_total / n as f32;
    let gap_len = gap_total / (n as f32 - 1.0).max(1.0);
    for i in 0..n {
        let offset = i as f32 * (dash_len + gap_len);
        if horizontal {
            rect(quads, x + offset, y, dash_len, h, color);
        } else {
            rect(quads, x, y + offset, w, dash_len, color);
        }
    }
}

#[rustfmt::skip]
fn sides_for(ch: char) -> Sides {
    use Weight::{Double as D, Heavy as H, Light as L, None as N};
    let mk = |l, r, u, d, dash| Sides { left: l, right: r, up: u, down: d, dash };
    let solid = Dash::Solid;
    match ch {
        // Horizontal/vertical
        '─' => mk(L, L, N, N, solid),
        '━' => mk(H, H, N, N, solid),
        '│' => mk(N, N, L, L, solid),
        '┃' => mk(N, N, H, H, solid),
        // Dashed (treated as same weight, drawn with N-segment dash pattern)
        '┄' => mk(L, L, N, N, Dash::Triple),
        '┅' => mk(H, H, N, N, Dash::Triple),
        '┆' => mk(N, N, L, L, Dash::Triple),
        '┇' => mk(N, N, H, H, Dash::Triple),
        '┈' => mk(L, L, N, N, Dash::Quad),
        '┉' => mk(H, H, N, N, Dash::Quad),
        '┊' => mk(N, N, L, L, Dash::Quad),
        '┋' => mk(N, N, H, H, Dash::Quad),
        '╌' => mk(L, L, N, N, Dash::Double),
        '╍' => mk(H, H, N, N, Dash::Double),
        '╎' => mk(N, N, L, L, Dash::Double),
        '╏' => mk(N, N, H, H, Dash::Double),

        // Corners (light)
        '┌' => mk(N, L, N, L, solid),
        '┐' => mk(L, N, N, L, solid),
        '└' => mk(N, L, L, N, solid),
        '┘' => mk(L, N, L, N, solid),
        // Corners (heavy)
        '┏' => mk(N, H, N, H, solid),
        '┓' => mk(H, N, N, H, solid),
        '┗' => mk(N, H, H, N, solid),
        '┛' => mk(H, N, H, N, solid),
        // Mixed-weight corners
        '┍' => mk(N, H, N, L, solid),
        '┎' => mk(N, L, N, H, solid),
        '┑' => mk(H, N, N, L, solid),
        '┒' => mk(L, N, N, H, solid),
        '┕' => mk(N, H, L, N, solid),
        '┖' => mk(N, L, H, N, solid),
        '┙' => mk(H, N, L, N, solid),
        '┚' => mk(L, N, H, N, solid),
        // Rounded corners — drawn as plain corners
        '╭' => mk(N, L, N, L, solid),
        '╮' => mk(L, N, N, L, solid),
        '╯' => mk(L, N, L, N, solid),
        '╰' => mk(N, L, L, N, solid),

        // T-junctions (light)
        '├' => mk(N, L, L, L, solid),
        '┤' => mk(L, N, L, L, solid),
        '┬' => mk(L, L, N, L, solid),
        '┴' => mk(L, L, L, N, solid),
        '┼' => mk(L, L, L, L, solid),
        // T-junctions (heavy)
        '┣' => mk(N, H, H, H, solid),
        '┫' => mk(H, N, H, H, solid),
        '┳' => mk(H, H, N, H, solid),
        '┻' => mk(H, H, H, N, solid),
        '╋' => mk(H, H, H, H, solid),
        // T-junctions (mixed light/heavy) — common subset
        '┝' => mk(N, H, L, L, solid),
        '┞' => mk(N, L, H, L, solid),
        '┟' => mk(N, L, L, H, solid),
        '┠' => mk(N, L, H, H, solid),
        '┡' => mk(N, H, H, L, solid),
        '┢' => mk(N, H, L, H, solid),
        '┥' => mk(H, N, L, L, solid),
        '┦' => mk(L, N, H, L, solid),
        '┧' => mk(L, N, L, H, solid),
        '┨' => mk(L, N, H, H, solid),
        '┩' => mk(H, N, H, L, solid),
        '┪' => mk(H, N, L, H, solid),
        '┭' => mk(H, L, N, L, solid),
        '┮' => mk(L, H, N, L, solid),
        '┯' => mk(H, H, N, L, solid),
        '┰' => mk(L, L, N, H, solid),
        '┱' => mk(H, L, N, H, solid),
        '┲' => mk(L, H, N, H, solid),
        '┵' => mk(H, L, L, N, solid),
        '┶' => mk(L, H, L, N, solid),
        '┷' => mk(H, H, L, N, solid),
        '┸' => mk(L, L, H, N, solid),
        '┹' => mk(H, L, H, N, solid),
        '┺' => mk(L, H, H, N, solid),
        '┽' => mk(H, L, L, L, solid),
        '┾' => mk(L, H, L, L, solid),
        '┿' => mk(H, H, L, L, solid),
        '╀' => mk(L, L, H, L, solid),
        '╁' => mk(L, L, L, H, solid),
        '╂' => mk(L, L, H, H, solid),
        '╃' => mk(H, L, H, L, solid),
        '╄' => mk(L, H, H, L, solid),
        '╅' => mk(H, L, L, H, solid),
        '╆' => mk(L, H, L, H, solid),
        '╇' => mk(H, H, H, L, solid),
        '╈' => mk(H, H, L, H, solid),
        '╉' => mk(H, L, H, H, solid),
        '╊' => mk(L, H, H, H, solid),

        // Light-heavy half lines U+2574..257B
        '╴' => mk(L, N, N, N, solid),
        '╵' => mk(N, N, L, N, solid),
        '╶' => mk(N, L, N, N, solid),
        '╷' => mk(N, N, N, L, solid),
        '╸' => mk(H, N, N, N, solid),
        '╹' => mk(N, N, H, N, solid),
        '╺' => mk(N, H, N, N, solid),
        '╻' => mk(N, N, N, H, solid),
        '╼' => mk(L, H, N, N, solid),
        '╽' => mk(N, N, L, H, solid),
        '╾' => mk(H, L, N, N, solid),
        '╿' => mk(N, N, H, L, solid),

        // Double lines
        '═' => mk(D, D, N, N, solid),
        '║' => mk(N, N, D, D, solid),
        '╔' => mk(N, D, N, D, solid),
        '╗' => mk(D, N, N, D, solid),
        '╚' => mk(N, D, D, N, solid),
        '╝' => mk(D, N, D, N, solid),
        '╠' => mk(N, D, D, D, solid),
        '╣' => mk(D, N, D, D, solid),
        '╦' => mk(D, D, N, D, solid),
        '╩' => mk(D, D, D, N, solid),
        '╬' => mk(D, D, D, D, solid),
        // Single/double mixes
        '╒' => mk(N, D, N, L, solid),
        '╓' => mk(N, L, N, D, solid),
        '╕' => mk(D, N, N, L, solid),
        '╖' => mk(L, N, N, D, solid),
        '╘' => mk(N, D, L, N, solid),
        '╙' => mk(N, L, D, N, solid),
        '╛' => mk(D, N, L, N, solid),
        '╜' => mk(L, N, D, N, solid),
        '╞' => mk(N, D, L, L, solid),
        '╟' => mk(N, L, D, D, solid),
        '╡' => mk(D, N, L, L, solid),
        '╢' => mk(L, N, D, D, solid),
        '╤' => mk(D, D, N, L, solid),
        '╥' => mk(L, L, N, D, solid),
        '╧' => mk(D, D, L, N, solid),
        '╨' => mk(L, L, D, N, solid),
        '╪' => mk(D, D, L, L, solid),
        '╫' => mk(L, L, D, D, solid),

        _ => Sides::none(),
    }
}

#[allow(clippy::too_many_arguments)]
fn push_block_element(
    quads: &mut Vec<Quad>,
    ch: char,
    x: f32,
    y: f32,
    cell_w: f32,
    cell_h: f32,
    color: [f32; 4],
) {
    // Horizontal block elements (lower N/8) and vertical (left N/8).
    let frac = |n: f32| -> f32 { (n * cell_h / 8.0).round() };
    let fracw = |n: f32| -> f32 { (n * cell_w / 8.0).round() };

    match ch {
        // Upper half block
        '▀' => rect(quads, x, y, cell_w, (cell_h * 0.5).round(), color),
        // Lower N/8 blocks (▁..█)
        '▁' => {
            let h = frac(1.0);
            rect(quads, x, y + cell_h - h, cell_w, h, color);
        }
        '▂' => {
            let h = frac(2.0);
            rect(quads, x, y + cell_h - h, cell_w, h, color);
        }
        '▃' => {
            let h = frac(3.0);
            rect(quads, x, y + cell_h - h, cell_w, h, color);
        }
        '▄' => {
            let h = frac(4.0);
            rect(quads, x, y + cell_h - h, cell_w, h, color);
        }
        '▅' => {
            let h = frac(5.0);
            rect(quads, x, y + cell_h - h, cell_w, h, color);
        }
        '▆' => {
            let h = frac(6.0);
            rect(quads, x, y + cell_h - h, cell_w, h, color);
        }
        '▇' => {
            let h = frac(7.0);
            rect(quads, x, y + cell_h - h, cell_w, h, color);
        }
        '█' => rect(quads, x, y, cell_w, cell_h, color),
        // Left N/8 blocks (▉..▏)
        '▉' => {
            let w = fracw(7.0);
            rect(quads, x, y, w, cell_h, color);
        }
        '▊' => {
            let w = fracw(6.0);
            rect(quads, x, y, w, cell_h, color);
        }
        '▋' => {
            let w = fracw(5.0);
            rect(quads, x, y, w, cell_h, color);
        }
        '▌' => {
            let w = fracw(4.0);
            rect(quads, x, y, w, cell_h, color);
        }
        '▍' => {
            let w = fracw(3.0);
            rect(quads, x, y, w, cell_h, color);
        }
        '▎' => {
            let w = fracw(2.0);
            rect(quads, x, y, w, cell_h, color);
        }
        '▏' => {
            let w = fracw(1.0);
            rect(quads, x, y, w, cell_h, color);
        }
        // Right half block
        '▐' => {
            let w = (cell_w * 0.5).round();
            rect(quads, x + cell_w - w, y, w, cell_h, color);
        }
        // Upper 1/8, right 1/8
        '▔' => {
            let h = frac(1.0);
            rect(quads, x, y, cell_w, h, color);
        }
        '▕' => {
            let w = fracw(1.0);
            rect(quads, x + cell_w - w, y, w, cell_h, color);
        }
        // Shade blocks: dither dot pattern at light/medium/dark coverage
        '░' | '▒' | '▓' => push_shade(quads, ch, x, y, cell_w, cell_h, color),
        // Quadrant blocks
        '▖' => {
            let hw = (cell_w * 0.5).round();
            let hh = (cell_h * 0.5).round();
            rect(quads, x, y + cell_h - hh, hw, hh, color);
        }
        '▗' => {
            let hw = (cell_w * 0.5).round();
            let hh = (cell_h * 0.5).round();
            rect(quads, x + cell_w - hw, y + cell_h - hh, hw, hh, color);
        }
        '▘' => {
            let hw = (cell_w * 0.5).round();
            let hh = (cell_h * 0.5).round();
            rect(quads, x, y, hw, hh, color);
        }
        '▝' => {
            let hw = (cell_w * 0.5).round();
            let hh = (cell_h * 0.5).round();
            rect(quads, x + cell_w - hw, y, hw, hh, color);
        }
        '▙' => {
            let hw = (cell_w * 0.5).round();
            let hh = (cell_h * 0.5).round();
            rect(quads, x, y, hw, cell_h, color);
            rect(quads, x + hw, y + cell_h - hh, cell_w - hw, hh, color);
        }
        '▚' => {
            let hw = (cell_w * 0.5).round();
            let hh = (cell_h * 0.5).round();
            rect(quads, x, y, hw, hh, color);
            rect(quads, x + hw, y + cell_h - hh, cell_w - hw, hh, color);
        }
        '▛' => {
            let hw = (cell_w * 0.5).round();
            let hh = (cell_h * 0.5).round();
            rect(quads, x, y, cell_w, hh, color);
            rect(quads, x, y + hh, hw, cell_h - hh, color);
        }
        '▜' => {
            let hw = (cell_w * 0.5).round();
            let hh = (cell_h * 0.5).round();
            rect(quads, x, y, cell_w, hh, color);
            rect(quads, x + hw, y + hh, cell_w - hw, cell_h - hh, color);
        }
        '▟' => {
            let hw = (cell_w * 0.5).round();
            let hh = (cell_h * 0.5).round();
            rect(quads, x + hw, y, cell_w - hw, cell_h, color);
            rect(quads, x, y + hh, hw, cell_h - hh, color);
        }
        '▞' => {
            let hw = (cell_w * 0.5).round();
            let hh = (cell_h * 0.5).round();
            rect(quads, x + hw, y, cell_w - hw, hh, color);
            rect(quads, x, y + hh, hw, cell_h - hh, color);
        }
        _ => {}
    }
}

fn push_shade(
    quads: &mut Vec<Quad>,
    ch: char,
    x: f32,
    y: f32,
    cell_w: f32,
    cell_h: f32,
    color: [f32; 4],
) {
    // Use alpha so the underlying cell bg shows through, like a true shade.
    let alpha = match ch {
        '░' => 0.25,
        '▒' => 0.5,
        '▓' => 0.75,
        _ => 1.0,
    };
    let mut c = color;
    c[3] *= alpha;
    rect(quads, x, y, cell_w, cell_h, c);
}

#[allow(clippy::too_many_arguments)]
fn push_braille(
    quads: &mut Vec<Quad>,
    ch: char,
    x: f32,
    y: f32,
    cell_w: f32,
    cell_h: f32,
    color: [f32; 4],
    scale: f32,
) {
    // Bit layout (Unicode dot ordering):
    //   bit 0 -> dot 1 (top-left)
    //   bit 1 -> dot 2 (mid-upper-left)
    //   bit 2 -> dot 3 (mid-lower-left)
    //   bit 6 -> dot 7 (bottom-left)
    //   bit 3 -> dot 4 (top-right)
    //   bit 4 -> dot 5 (mid-upper-right)
    //   bit 5 -> dot 6 (mid-lower-right)
    //   bit 7 -> dot 8 (bottom-right)
    let mask = (ch as u32 - 0x2800) as u8;
    let dot_w = (scale * 1.6).round().max(1.0);
    let dot_h = (scale * 1.6).round().max(1.0);

    // Column x-centres (1/4 and 3/4 across the cell), dot row y-centres at
    // 1/8, 3/8, 5/8, 7/8 of cell_h.
    let cx_l = x + cell_w * 0.25;
    let cx_r = x + cell_w * 0.75;
    let cy_rows = [
        y + cell_h * 0.125,
        y + cell_h * 0.375,
        y + cell_h * 0.625,
        y + cell_h * 0.875,
    ];

    let dots: [(u8, f32, f32); 8] = [
        (0, cx_l, cy_rows[0]),
        (1, cx_l, cy_rows[1]),
        (2, cx_l, cy_rows[2]),
        (6, cx_l, cy_rows[3]),
        (3, cx_r, cy_rows[0]),
        (4, cx_r, cy_rows[1]),
        (5, cx_r, cy_rows[2]),
        (7, cx_r, cy_rows[3]),
    ];

    for (bit, dx, dy) in dots {
        if mask & (1 << bit) != 0 {
            rect(
                quads,
                (dx - dot_w * 0.5).round(),
                (dy - dot_h * 0.5).round(),
                dot_w,
                dot_h,
                color,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CW: f32 = 10.0;
    const CH: f32 = 20.0;
    const SCALE: f32 = 1.0;
    const FG: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

    fn render(ch: char, x: f32, y: f32) -> Vec<Quad> {
        let mut q = Vec::new();
        push_quads(&mut q, ch, x, y, CW, CH, FG, SCALE);
        q
    }

    fn bounds_x(q: &Quad) -> (f32, f32) {
        (q.rect[0], q.rect[0] + q.rect[2])
    }
    fn bounds_y(q: &Quad) -> (f32, f32) {
        (q.rect[1], q.rect[1] + q.rect[3])
    }

    #[test]
    fn is_handled_range() {
        assert!(is_handled('─'));
        assert!(is_handled('│'));
        assert!(is_handled('█'));
        assert!(is_handled('▒'));
        assert!(is_handled('⠁'));
        assert!(is_handled('⣿'));
        // Outside the handled ranges.
        assert!(!is_handled('a'));
        assert!(!is_handled(' '));
        assert!(!is_handled('▲'));
        assert!(!is_handled('●'));
    }

    /// Pick a y in the middle of the line stroke (cell centre), then return
    /// (min_x_start, max_x_end) across all quads of `cell` that contain it.
    /// Lets us check "does the stroke cover the full cell width at row y?"
    /// regardless of how many sub-segments the renderer used.
    fn x_span_at(cell: &[Quad], y: f32) -> Option<(f32, f32)> {
        cell.iter()
            .filter(|q| y >= q.rect[1] && y <= q.rect[1] + q.rect[3])
            .fold(None, |acc, q| {
                let (x0, x1) = bounds_x(q);
                Some(match acc {
                    None => (x0, x1),
                    Some((a, b)) => (a.min(x0), b.max(x1)),
                })
            })
    }

    fn y_span_at(cell: &[Quad], x: f32) -> Option<(f32, f32)> {
        cell.iter()
            .filter(|q| x >= q.rect[0] && x <= q.rect[0] + q.rect[2])
            .fold(None, |acc, q| {
                let (y0, y1) = bounds_y(q);
                Some(match acc {
                    None => (y0, y1),
                    Some((a, b)) => (a.min(y0), b.max(y1)),
                })
            })
    }

    /// Two horizontally-adjacent `─` cells must produce horizontal-line
    /// quads that meet at the cell boundary. The point of this whole module
    /// is to avoid the visible gap that fonts leave here.
    #[test]
    fn adjacent_horizontal_lines_have_no_gap() {
        let left = render('─', 0.0, 0.0);
        let right = render('─', CW, 0.0);
        let (_, lx1) = x_span_at(&left, CH / 2.0).expect("left stroke at cy");
        let (rx0, _) = x_span_at(&right, CH / 2.0).expect("right stroke at cy");
        assert!(
            lx1 >= rx0,
            "horizontal lines have a gap: left ends at {lx1}, right starts at {rx0}"
        );
    }

    /// Same for vertical `│` cells stacked above each other.
    #[test]
    fn adjacent_vertical_lines_have_no_gap() {
        let top = render('│', 0.0, 0.0);
        let bot = render('│', 0.0, CH);
        let (_, ty1) = y_span_at(&top, CW / 2.0).expect("top stroke at cx");
        let (by0, _) = y_span_at(&bot, CW / 2.0).expect("bot stroke at cx");
        assert!(
            ty1 >= by0,
            "vertical lines have a gap: top ends at {ty1}, bot starts at {by0}"
        );
    }

    /// A `┌` corner's right-going stroke must reach the right cell edge,
    /// and its down-going stroke must reach the bottom cell edge. This is
    /// what lets the next cell's `─` or the cell below's `│` join cleanly.
    #[test]
    fn corner_strokes_reach_cell_edges() {
        let q = render('┌', 0.0, 0.0);
        let right_reaches = q.iter().any(|q| (q.rect[0] + q.rect[2] - CW).abs() < 0.01);
        let down_reaches = q.iter().any(|q| (q.rect[1] + q.rect[3] - CH).abs() < 0.01);
        assert!(right_reaches, "┌ does not extend to right edge: {q:?}");
        assert!(down_reaches, "┌ does not extend to bottom edge: {q:?}");
    }

    #[test]
    fn cross_reaches_all_four_edges() {
        let q = render('┼', 0.0, 0.0);
        assert!(q.iter().any(|r| r.rect[0] <= 0.01), "no left reach");
        assert!(
            q.iter().any(|r| (r.rect[0] + r.rect[2] - CW).abs() < 0.01),
            "no right reach"
        );
        assert!(q.iter().any(|r| r.rect[1] <= 0.01), "no up reach");
        assert!(
            q.iter().any(|r| (r.rect[1] + r.rect[3] - CH).abs() < 0.01),
            "no down reach"
        );
    }

    /// `█` fills the entire cell exactly — exactly one quad covering the cell rect.
    #[test]
    fn full_block_fills_cell() {
        let q = render('█', 0.0, 0.0);
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].rect, [0.0, 0.0, CW, CH]);
    }

    /// The lower-half block `▄` covers exactly the bottom half of the cell,
    /// and the upper-half `▀` covers the top half — together they tile
    /// without gap or overlap.
    #[test]
    fn half_blocks_tile_vertically() {
        let upper = render('▀', 0.0, 0.0);
        let lower = render('▄', 0.0, 0.0);
        assert_eq!(upper.len(), 1);
        assert_eq!(lower.len(), 1);
        let (_, uy1) = bounds_y(&upper[0]);
        let (ly0, _) = bounds_y(&lower[0]);
        assert!(
            (uy1 - ly0).abs() < 0.01,
            "half-block seam mismatch: upper ends at {uy1}, lower starts at {ly0}"
        );
    }

    /// Two horizontally-adjacent `█` cells must produce quads that meet at
    /// the cell boundary (no vertical gap between solid block columns).
    #[test]
    fn adjacent_full_blocks_touch() {
        let left = render('█', 0.0, 0.0);
        let right = render('█', CW, 0.0);
        let (_, lx1) = bounds_x(&left[0]);
        let (rx0, _) = bounds_x(&right[0]);
        assert!(
            (lx1 - rx0).abs() < 0.01,
            "adjacent full blocks have a gap: {lx1} vs {rx0}"
        );
    }

    /// All 8 braille dots active (`⣿`, U+28FF) produces 8 dot quads.
    #[test]
    fn braille_all_dots() {
        let q = render('⣿', 0.0, 0.0);
        assert_eq!(q.len(), 8, "expected 8 dots for ⣿, got {}", q.len());
    }

    /// Empty braille `⠀` (U+2800) produces no quads.
    #[test]
    fn braille_empty_no_dots() {
        let q = render('⠀', 0.0, 0.0);
        assert!(q.is_empty(), "expected 0 dots for ⠀, got {}", q.len());
    }

    /// Each braille bit toggles exactly one dot quad.
    #[test]
    fn braille_bit_count_matches() {
        for mask in 0u32..=0xFFu32 {
            let ch = char::from_u32(0x2800 + mask).unwrap();
            let q = render(ch, 0.0, 0.0);
            assert_eq!(
                q.len(),
                mask.count_ones() as usize,
                "braille mask {mask:08b} produced {} quads, expected {}",
                q.len(),
                mask.count_ones()
            );
        }
    }

    /// Every code point in U+2500..=U+257F that we map should produce at
    /// least one quad. (Catches accidental table drops during edits.)
    #[test]
    fn mapped_box_drawing_chars_emit_quads() {
        // Spot-check the chars that show up in normal TUI use.
        for ch in [
            '─', '━', '│', '┃', '┌', '┐', '└', '┘', '├', '┤', '┬', '┴', '┼', '═', '║', '╔', '╗',
            '╚', '╝', '╠', '╣', '╦', '╩', '╬', '╴', '╵', '╶', '╷', '╭', '╮', '╯', '╰',
        ] {
            let q = render(ch, 0.0, 0.0);
            assert!(!q.is_empty(), "no quads emitted for {ch:?}");
        }
    }

    /// Heavy `━` should be visibly thicker than light `─`. The whole point
    /// of the heavy variant is the extra weight; locking it in prevents a
    /// future refactor from collapsing them.
    #[test]
    fn heavy_lines_are_thicker_than_light() {
        let light = render('─', 0.0, 0.0);
        let heavy = render('━', 0.0, 0.0);
        let l_h = light.iter().map(|q| q.rect[3]).fold(0.0_f32, f32::max);
        let h_h = heavy.iter().map(|q| q.rect[3]).fold(0.0_f32, f32::max);
        assert!(h_h > l_h, "heavy ({h_h}) not thicker than light ({l_h})");
    }
}
