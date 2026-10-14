//! Skin colour themes (skins sheet): Fiddlesticks' red effects and icons turned to a skin's or chroma's colour.

use crate::sheet;

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    /// degrees the base red becomes; negative keeps the hue
    pub hue: f32,
    pub sat: f32,
    pub val: f32,
}

/// The theme of skin `num` (a chroma of `parent` when they differ), named `name` in the client.
pub fn for_skin(num: u32, parent: u32, name: &str) -> Option<Theme> {
    let lower = name.to_lowercase();
    let row = sheet::SKIN_THEMES
        .iter()
        .filter(|_| num != parent)
        .find(|t| t.matches.strip_prefix("chroma:").map(|k| lower.contains(k)).unwrap_or(false))
        .or_else(|| sheet::SKIN_THEMES.iter().find(|t| t.matches == format!("skin:{parent}")))?;
    let t = Theme { hue: row.hue, sat: row.sat, val: row.val };
    (t.hue >= 0.0 || t.sat != 1.0 || t.val != 1.0).then_some(t)
}

/// An RGB colour (0..1) recoloured by a theme: saturated colours take its hue, all take its saturation/value.
pub fn recolor(c: [f32; 3], t: &Theme) -> [f32; 3] {
    let (mx, mn) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
    let v = mx;
    let s = if mx > 0.0 { (mx - mn) / mx } else { 0.0 };
    let mut h = if mx == mn {
        0.0
    } else if mx == c[0] {
        60.0 * ((c[1] - c[2]) / (mx - mn))
    } else if mx == c[1] {
        60.0 * ((c[2] - c[0]) / (mx - mn)) + 120.0
    } else {
        60.0 * ((c[0] - c[1]) / (mx - mn)) + 240.0
    };
    if t.hue >= 0.0 && s > 0.08 {
        h += t.hue;
    }
    let h = h.rem_euclid(360.0);
    let (s, v) = ((s * t.sat).clamp(0.0, 1.0), (v * t.val).clamp(0.0, 1.0));
    let f = |n: f32| {
        let k = (n + h / 60.0) % 6.0;
        v - v * s * k.min(4.0 - k).clamp(0.0, 1.0)
    };
    [f(5.0), f(3.0), f(1.0)]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn red_turns_blue_and_grey_stays_grey() {
        let t = Theme { hue: 240.0, sat: 1.0, val: 1.0 };
        let b = recolor([1.0, 0.0, 0.0], &t);
        assert!(b[2] > 0.99 && b[0] < 0.01 && b[1] < 0.01, "{b:?}");
        let g = recolor([0.5, 0.5, 0.5], &t);
        assert!((g[0] - 0.5).abs() < 1e-4 && (g[2] - 0.5).abs() < 1e-4, "{g:?}");
        let keep = Theme { hue: -1.0, sat: 0.0, val: 1.0 };
        let d = recolor([1.0, 0.2, 0.2], &keep);
        assert!((d[0] - d[1]).abs() < 1e-4, "{d:?}");
    }
}
