//! Translation of declared style properties (camelCase, CSS-like) into a Taffy
//! layout style, box paint parameters and inherited text style.

use taffy::geometry::Point;
use taffy::prelude::*;
use taffy::style::{GridTemplateComponent, Overflow};

use crate::color::Color;
use crate::value::{Length, PropValue};

/// Looks up a (possibly animated) style property of a node.
pub trait StyleSource {
    fn style(&self, key: &str) -> Option<PropValue>;

    fn num(&self, key: &str) -> Option<f32> {
        self.style(key).and_then(|v| v.as_f32())
    }

    fn string(&self, key: &str) -> Option<String> {
        self.style(key).and_then(|v| match v {
            PropValue::String(s) => Some(s),
            PropValue::Number(n) => Some(n.to_string()),
            _ => None,
        })
    }
}

// ───────────────────────────── layout ─────────────────────────────

fn dim(v: Option<PropValue>) -> Dimension {
    match v.as_ref().and_then(Length::parse) {
        Some(Length::Px(p)) => length(p),
        Some(Length::Percent(p)) => percent(p / 100.0),
        _ => auto(),
    }
}

fn lpa(v: Option<PropValue>) -> LengthPercentageAuto {
    match v.as_ref().and_then(Length::parse) {
        Some(Length::Px(p)) => length(p),
        Some(Length::Percent(p)) => percent(p / 100.0),
        _ => auto(),
    }
}

/// Margins default to 0 (an `auto` margin would centre the item).
fn margin(v: Option<PropValue>) -> LengthPercentageAuto {
    match v {
        None => length(0.0),
        v => lpa(v),
    }
}

fn lp(v: Option<PropValue>) -> LengthPercentage {
    match v.as_ref().and_then(Length::parse) {
        Some(Length::Percent(p)) => percent(p / 100.0),
        Some(Length::Px(p)) => length(p),
        _ => length(0.0),
    }
}

/// Expands CSS box shorthands: `8`, `"8 16"`, `"8 16 4"`, `"8 16 4 2"` → [top, right, bottom, left].
fn box_shorthand(v: &PropValue) -> [PropValue; 4] {
    let parts: Vec<PropValue> = match v {
        PropValue::String(s) => s
            .split_whitespace()
            .map(|p| PropValue::String(p.to_string()))
            .collect(),
        PropValue::List(items) => items.clone(),
        other => vec![other.clone()],
    };
    let p = |i: usize| parts.get(i).cloned().unwrap_or(PropValue::Null);
    match parts.len() {
        0 => [
            PropValue::Null,
            PropValue::Null,
            PropValue::Null,
            PropValue::Null,
        ],
        1 => [p(0), p(0), p(0), p(0)],
        2 => [p(0), p(1), p(0), p(1)],
        3 => [p(0), p(1), p(2), p(1)],
        _ => [p(0), p(1), p(2), p(3)],
    }
}

fn sides(src: &dyn StyleSource, base: &str) -> [Option<PropValue>; 4] {
    let mut out: [Option<PropValue>; 4] = Default::default();
    if let Some(v) = src.style(base) {
        let [t, r, b, l] = box_shorthand(&v);
        out = [Some(t), Some(r), Some(b), Some(l)];
    }
    for (i, axis) in ["Top", "Right", "Bottom", "Left"].iter().enumerate() {
        if let Some(v) = src.style(&format!("{base}{axis}")) {
            out[i] = Some(v);
        }
    }
    for (axis, idx) in [("Horizontal", [1usize, 3]), ("Vertical", [0, 2])] {
        if let Some(v) = src.style(&format!("{base}{axis}")) {
            for i in idx {
                if out[i].is_none() {
                    out[i] = Some(v.clone());
                }
            }
        }
    }
    out
}

fn track(token: &str) -> Option<TrackSizingFunction> {
    let t = token.trim();
    if t == "auto" {
        return Some(auto());
    }
    if let Some(f) = t.strip_suffix("fr") {
        return f.parse::<f32>().ok().map(fr);
    }
    if let Some(p) = t.strip_suffix('%') {
        return p.parse::<f32>().ok().map(|v| percent(v / 100.0));
    }
    if let Some(inner) = t.strip_prefix("minmax(").and_then(|r| r.strip_suffix(')')) {
        let (a, b) = inner.split_once(',')?;
        let min: MinTrackSizingFunction = match a.trim() {
            "auto" => auto(),
            x => length(crate::value::parse_length_px(x)? as f32),
        };
        let max: MaxTrackSizingFunction = match b.trim() {
            "auto" => auto(),
            x if x.ends_with("fr") => fr(x.trim_end_matches("fr").parse::<f32>().ok()?),
            x => length(crate::value::parse_length_px(x)? as f32),
        };
        return Some(minmax(min, max));
    }
    crate::value::parse_length_px(t).map(|v| length(v as f32))
}

/// Splits a track list on whitespace while keeping parenthesised groups together.
fn split_tracks(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.push(c)
            }
            ')' => {
                depth -= 1;
                cur.push(c)
            }
            c if c.is_whitespace() && depth == 0 => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Parses `"1fr 2fr 80px"`, `"repeat(3, 1fr)"` or a number (N equal columns).
pub fn parse_grid_template(v: &PropValue) -> Vec<GridTemplateComponent<String>> {
    let s = match v {
        PropValue::Number(n) => format!("repeat({}, 1fr)", *n as u32),
        PropValue::String(s) => s.clone(),
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    for tok in split_tracks(&s) {
        if let Some(inner) = tok
            .strip_prefix("repeat(")
            .and_then(|r| r.strip_suffix(')'))
        {
            if let Some((count, body)) = inner.split_once(',')
                && let Ok(n) = count.trim().parse::<u16>()
            {
                let tracks: Vec<TrackSizingFunction> =
                    split_tracks(body).iter().filter_map(|t| track(t)).collect();
                for _ in 0..n {
                    for t in &tracks {
                        out.push(GridTemplateComponent::Single(*t));
                    }
                }
            }
        } else if let Some(t) = track(&tok) {
            out.push(GridTemplateComponent::Single(t));
        }
    }
    out
}

/// Parses `"span 2"`, `"1 / 3"`, `"2"`, `2`.
fn grid_line(v: Option<PropValue>) -> Line<GridPlacement<String>> {
    let Some(v) = v else {
        return Line {
            start: GridPlacement::Auto,
            end: GridPlacement::Auto,
        };
    };
    let s = v.to_string();
    let one = |p: &str| -> GridPlacement<String> {
        let p = p.trim();
        if let Some(n) = p.strip_prefix("span") {
            n.trim()
                .parse::<u16>()
                .map(span)
                .unwrap_or(GridPlacement::Auto)
        } else {
            p.parse::<i16>().map(line).unwrap_or(GridPlacement::Auto)
        }
    };
    match s.split_once('/') {
        Some((a, b)) => Line {
            start: one(a),
            end: one(b),
        },
        None => Line {
            start: one(&s),
            end: GridPlacement::Auto,
        },
    }
}

/// Builds the Taffy style of a box from its declared properties.
pub fn layout_style(src: &dyn StyleSource) -> Style<String> {
    let mut st: Style<String> = Style {
        display: match src.string("display").as_deref() {
            Some("none") => Display::None,
            Some("grid") => Display::Grid,
            Some("block") => Display::Block,
            _ => Display::Flex,
        },
        position: match src.string("position").as_deref() {
            Some("absolute") => Position::Absolute,
            _ => Position::Relative,
        },
        ..Default::default()
    };

    st.size = Size {
        width: dim(src.style("width")),
        height: dim(src.style("height")),
    };
    st.min_size = Size {
        width: lpa(src.style("minWidth")),
        height: lpa(src.style("minHeight")),
    };
    st.max_size = Size {
        width: lpa(src.style("maxWidth")),
        height: lpa(src.style("maxHeight")),
    };
    st.aspect_ratio = src.num("aspectRatio");

    let inset = |k: &str| src.style(k).or_else(|| src.style("inset"));
    st.inset = Rect {
        top: lpa(inset("top")),
        right: lpa(inset("right")),
        bottom: lpa(inset("bottom")),
        left: lpa(inset("left")),
    };

    let [mt, mr, mb, ml] = sides(src, "margin");
    st.margin = Rect {
        top: margin(mt),
        right: margin(mr),
        bottom: margin(mb),
        left: margin(ml),
    };
    let [pt, pr, pb, pl] = sides(src, "padding");
    st.padding = Rect {
        top: lp(pt),
        right: lp(pr),
        bottom: lp(pb),
        left: lp(pl),
    };
    if let Some(w) = src.num("borderWidth") {
        st.border = Rect {
            top: length(w),
            right: length(w),
            bottom: length(w),
            left: length(w),
        };
    }

    let gap = src.style("gap");
    st.gap = Size {
        width: lp(src.style("columnGap").or(gap.clone())),
        height: lp(src.style("rowGap").or(gap)),
    };

    st.flex_direction = match src.string("flexDirection").as_deref() {
        Some("column") => FlexDirection::Column,
        Some("row-reverse") => FlexDirection::RowReverse,
        Some("column-reverse") => FlexDirection::ColumnReverse,
        _ => FlexDirection::Row,
    };
    st.flex_wrap = match src.string("flexWrap").as_deref() {
        Some("wrap") => FlexWrap::Wrap,
        Some("wrap-reverse") => FlexWrap::WrapReverse,
        _ => FlexWrap::NoWrap,
    };
    if let Some(f) = src.style("flex") {
        // `flex: 1` → grow 1, shrink 1, basis 0
        if let Some(n) = f.as_f32() {
            st.flex_grow = n;
            st.flex_shrink = 1.0;
            st.flex_basis = length(0.0);
        }
    }
    if let Some(g) = src.num("flexGrow") {
        st.flex_grow = g;
    }
    if let Some(s) = src.num("flexShrink") {
        st.flex_shrink = s;
    }
    if let Some(b) = src.style("flexBasis") {
        st.flex_basis = dim(Some(b));
    }

    st.align_items =
        align_items(src.string("alignItems").as_deref()).unwrap_or(AlignItems::STRETCH);
    st.align_self = src.string("alignSelf").and_then(|s| align_items(Some(&s)));
    st.justify_items =
        align_items(src.string("justifyItems").as_deref()).unwrap_or(AlignItems::STRETCH);
    st.justify_self = src
        .string("justifySelf")
        .and_then(|s| align_items(Some(&s)));
    st.justify_content =
        content(src.string("justifyContent").as_deref()).unwrap_or(AlignContent::FLEX_START);
    st.align_content =
        content(src.string("alignContent").as_deref()).unwrap_or(AlignContent::STRETCH);

    if let Some(v) = src.style("gridTemplateColumns") {
        st.grid_template_columns = parse_grid_template(&v);
    }
    if let Some(v) = src.style("gridTemplateRows") {
        st.grid_template_rows = parse_grid_template(&v);
    }
    st.grid_column = grid_line(src.style("gridColumn"));
    st.grid_row = grid_line(src.style("gridRow"));

    if src.string("overflow").as_deref() == Some("hidden") {
        st.overflow = Point {
            x: Overflow::Hidden,
            y: Overflow::Hidden,
        };
    }
    st
}

fn align_items(s: Option<&str>) -> Option<AlignItems> {
    Some(match s? {
        "flex-start" | "start" => AlignItems::FLEX_START,
        "flex-end" | "end" => AlignItems::FLEX_END,
        "center" => AlignItems::CENTER,
        "baseline" => AlignItems::BASELINE,
        "stretch" => AlignItems::STRETCH,
        _ => return None,
    })
}

fn content(s: Option<&str>) -> Option<AlignContent> {
    Some(match s? {
        "flex-start" | "start" => AlignContent::FLEX_START,
        "flex-end" | "end" => AlignContent::FLEX_END,
        "center" => AlignContent::CENTER,
        "space-between" => AlignContent::SPACE_BETWEEN,
        "space-around" => AlignContent::SPACE_AROUND,
        "space-evenly" => AlignContent::SPACE_EVENLY,
        "stretch" => AlignContent::STRETCH,
        _ => return None,
    })
}

// ───────────────────────────── paint ─────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub enum Fill {
    Solid(Color),
    /// CSS angle convention: 0deg points up, 90deg points right.
    Linear {
        angle_deg: f32,
        stops: Vec<(f32, Color)>,
    },
    Radial {
        stops: Vec<(f32, Color)>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Shadow {
    pub dx: f32,
    pub dy: f32,
    pub blur: f32,
    pub color: Color,
}

/// Affine transform `[a b c d e f]` mapping (x, y) → (a·x + c·y + e, b·x + d·y + f).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform2D(pub [f32; 6]);

impl Transform2D {
    pub const IDENTITY: Self = Self([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    /// `self` applied first, then `other`.
    pub fn then(self, other: Self) -> Self {
        let [a1, b1, c1, d1, e1, f1] = self.0;
        let [a2, b2, c2, d2, e2, f2] = other.0;
        Self([
            a1 * a2 + b1 * c2,
            a1 * b2 + b1 * d2,
            c1 * a2 + d1 * c2,
            c1 * b2 + d1 * d2,
            e1 * a2 + f1 * c2 + e2,
            e1 * b2 + f1 * d2 + f2,
        ])
    }

    pub fn translate(x: f32, y: f32) -> Self {
        Self([1.0, 0.0, 0.0, 1.0, x, y])
    }

    pub fn scale(sx: f32, sy: f32) -> Self {
        Self([sx, 0.0, 0.0, sy, 0.0, 0.0])
    }

    pub fn rotate_deg(deg: f32) -> Self {
        let (s, c) = deg.to_radians().sin_cos();
        Self([c, s, -s, c, 0.0, 0.0])
    }

    pub fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        let [a, b, c, d, e, f] = self.0;
        (a * x + c * y + e, b * x + d * y + f)
    }

    /// The inverse transform, if the matrix is invertible.
    pub fn invert(&self) -> Option<Self> {
        let [a, b, c, d, e, f] = self.0;
        let det = a * d - b * c;
        if det.abs() < 1e-12 {
            return None;
        }
        Some(Self([
            d / det,
            -b / det,
            -c / det,
            a / det,
            (c * f - d * e) / det,
            (b * e - a * f) / det,
        ]))
    }

    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }
}

/// Parses CSS / SVG transform lists: `rotate(45deg) scale(1.2) translate(4px, 2px)`,
/// SVG `rotate(150 44 44)` and `matrix(a b c d e f)`. Functions apply left to right.
pub fn parse_transform(s: &str) -> Transform2D {
    let mut t = Transform2D::IDENTITY;
    let mut rest = s.trim();
    while let Some(open) = rest.find('(') {
        let name = rest[..open].trim().trim_start_matches(',').trim();
        let Some(close) = rest[open..].find(')') else {
            break;
        };
        let args: Vec<f32> = rest[open + 1..open + close]
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|a| !a.is_empty())
            .filter_map(|a| {
                let a = a.trim_end_matches("deg").trim_end_matches("px");
                a.parse::<f32>().ok()
            })
            .collect();
        let arg = |i: usize, d: f32| args.get(i).copied().unwrap_or(d);
        let m = match name {
            "translate" => Transform2D::translate(arg(0, 0.0), arg(1, 0.0)),
            "translateX" => Transform2D::translate(arg(0, 0.0), 0.0),
            "translateY" => Transform2D::translate(0.0, arg(0, 0.0)),
            "scale" => Transform2D::scale(arg(0, 1.0), arg(1, arg(0, 1.0))),
            "scaleX" => Transform2D::scale(arg(0, 1.0), 1.0),
            "scaleY" => Transform2D::scale(1.0, arg(0, 1.0)),
            "rotate" if args.len() >= 3 => {
                let (cx, cy) = (arg(1, 0.0), arg(2, 0.0));
                Transform2D::translate(-cx, -cy)
                    .then(Transform2D::rotate_deg(arg(0, 0.0)))
                    .then(Transform2D::translate(cx, cy))
            }
            "rotate" => Transform2D::rotate_deg(arg(0, 0.0)),
            "matrix" if args.len() == 6 => {
                Transform2D([args[0], args[1], args[2], args[3], args[4], args[5]])
            }
            _ => Transform2D::IDENTITY,
        };
        // CSS/SVG semantics: the rightmost function is applied to the element first.
        t = m.then(t);
        rest = &rest[open + close + 1..];
    }
    t
}

#[derive(Clone, Debug, PartialEq)]
pub struct BoxPaint {
    pub background: Option<Fill>,
    /// top-left, top-right, bottom-right, bottom-left
    pub radius: [f32; 4],
    pub border_width: f32,
    pub border_color: Option<Color>,
    pub opacity: f32,
    pub clip: bool,
    pub transform: Option<Transform2D>,
    pub shadow: Option<Shadow>,
    /// Frosted glass: the blurred wallpaper drawn behind the background.
    pub backdrop: Option<BackdropFilter>,
}

impl Default for BoxPaint {
    fn default() -> Self {
        Self {
            background: None,
            radius: [0.0; 4],
            border_width: 0.0,
            border_color: None,
            opacity: 1.0,
            clip: false,
            transform: None,
            shadow: None,
            backdrop: None,
        }
    }
}

pub fn parse_color(v: &str, current: Color) -> Option<Color> {
    if v.trim().eq_ignore_ascii_case("currentcolor") {
        Some(current)
    } else {
        Color::parse(v)
    }
}

/// Splits on commas that are not inside parentheses.
pub fn split_top_level(s: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.push(c)
            }
            ')' => {
                depth -= 1;
                cur.push(c)
            }
            c if c == sep && depth == 0 => out.push(std::mem::take(&mut cur).trim().to_string()),
            c => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

fn parse_stops(parts: &[String], current: Color) -> Vec<(f32, Color)> {
    let n = parts.len();
    let mut stops = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        // "color [pos%]" where color may contain spaces inside parentheses
        let (color_str, pos) = match p.rfind(|c: char| c.is_whitespace()) {
            Some(idx) if p[idx..].trim().ends_with('%') && !p[..idx].trim().is_empty() => (
                p[..idx].trim(),
                p[idx..]
                    .trim()
                    .trim_end_matches('%')
                    .parse::<f32>()
                    .ok()
                    .map(|v| v / 100.0),
            ),
            _ => (p.as_str(), None),
        };
        if let Some(c) = parse_color(color_str, current) {
            let default = if n > 1 {
                i as f32 / (n - 1) as f32
            } else {
                0.0
            };
            stops.push((pos.unwrap_or(default), c));
        }
    }
    stops
}

/// Parses a background value: a color, `linear-gradient(...)` or `radial-gradient(...)`.
pub fn parse_fill(s: &str, current: Color) -> Option<Fill> {
    let s = s.trim();
    if let Some(inner) = s
        .strip_prefix("linear-gradient(")
        .and_then(|r| r.strip_suffix(')'))
    {
        let parts = split_top_level(inner, ',');
        let (angle, stop_parts) = match parts.first() {
            Some(first) if first.ends_with("deg") => (
                first
                    .trim_end_matches("deg")
                    .trim()
                    .parse()
                    .unwrap_or(180.0),
                &parts[1..],
            ),
            Some(first) if first.starts_with("to ") => {
                let a = match first.as_str() {
                    "to top" => 0.0,
                    "to right" => 90.0,
                    "to bottom" => 180.0,
                    "to left" => 270.0,
                    "to top right" | "to right top" => 45.0,
                    "to bottom right" | "to right bottom" => 135.0,
                    "to bottom left" | "to left bottom" => 225.0,
                    "to top left" | "to left top" => 315.0,
                    _ => 180.0,
                };
                (a, &parts[1..])
            }
            _ => (180.0, &parts[..]),
        };
        let stops = parse_stops(stop_parts, current);
        return (!stops.is_empty()).then_some(Fill::Linear {
            angle_deg: angle,
            stops,
        });
    }
    if let Some(inner) = s
        .strip_prefix("radial-gradient(")
        .and_then(|r| r.strip_suffix(')'))
    {
        let parts = split_top_level(inner, ',');
        let start = usize::from(
            parts
                .first()
                .is_some_and(|p| p.starts_with("circle") || p.starts_with("ellipse")),
        );
        let stops = parse_stops(&parts[start..], current);
        return (!stops.is_empty()).then_some(Fill::Radial { stops });
    }
    parse_color(s, current).map(Fill::Solid)
}

/// Parses `box-shadow: 0 4px 12px rgba(0,0,0,0.3)` (first shadow only).
pub fn parse_shadow(s: &str, current: Color) -> Option<Shadow> {
    let first = split_top_level(s, ',').into_iter().next()?;
    let mut nums = Vec::new();
    let mut color = Color::rgba(0.0, 0.0, 0.0, 0.35);
    for tok in split_tracks(&first) {
        if let Some(n) = crate::value::parse_length_px(&tok) {
            nums.push(n as f32);
        } else if let Some(c) = parse_color(&tok, current) {
            color = c;
        }
    }
    if nums.len() < 2 {
        return None;
    }
    Some(Shadow {
        dx: nums[0],
        dy: nums[1],
        blur: nums.get(2).copied().unwrap_or(0.0),
        color,
    })
}

pub fn box_paint(src: &dyn StyleSource, current_color: Color) -> BoxPaint {
    let mut p = BoxPaint::default();
    let bg = src
        .string("background")
        .or_else(|| src.string("backgroundColor"));
    p.background = bg.and_then(|s| parse_fill(&s, current_color));
    if let Some(r) = src.style("borderRadius") {
        let [a, b, c, d] = box_shorthand(&r);
        let f = |v: PropValue| v.as_f32().unwrap_or(0.0);
        p.radius = [f(a), f(b), f(c), f(d)];
    }
    for (i, k) in [
        "borderTopLeftRadius",
        "borderTopRightRadius",
        "borderBottomRightRadius",
        "borderBottomLeftRadius",
    ]
    .iter()
    .enumerate()
    {
        if let Some(v) = src.num(k) {
            p.radius[i] = v;
        }
    }
    // `border: "1px solid rgba(...)"` shorthand
    if let Some(b) = src.string("border") {
        for tok in split_tracks(&b) {
            if let Some(n) = crate::value::parse_length_px(&tok) {
                p.border_width = n as f32;
            } else if let Some(c) = parse_color(&tok, current_color) {
                p.border_color = Some(c);
            }
        }
    }
    if let Some(w) = src.num("borderWidth") {
        p.border_width = w;
    }
    if let Some(c) = src
        .string("borderColor")
        .and_then(|s| parse_color(&s, current_color))
    {
        p.border_color = Some(c);
    }
    if p.border_width > 0.0 && p.border_color.is_none() {
        p.border_color = Some(current_color);
    }
    p.opacity = src.num("opacity").unwrap_or(1.0).clamp(0.0, 1.0);
    p.clip = src.string("overflow").as_deref() == Some("hidden");
    p.transform = src
        .string("transform")
        .map(|s| parse_transform(&s))
        .filter(|t| !t.is_identity());
    p.shadow = src
        .string("boxShadow")
        .and_then(|s| parse_shadow(&s, current_color));
    p.backdrop = src
        .string("backdropFilter")
        .and_then(|s| parse_backdrop_filter(&s));
    p
}

/// `backdropFilter` (not inherited): the wallpaper behind the box, blurred and
/// saturated like CSS `backdrop-filter`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BackdropFilter {
    /// Gaussian standard deviation in DIPs (the CSS `blur()` radius).
    pub blur: f32,
    /// Saturation factor; 1 leaves colours unchanged.
    pub saturate: f32,
}

/// Parses `"blur(16px) saturate(1.6)"`, either function alone (in any order,
/// `saturate` also as a percentage) or `"none"`. Repeated functions compose as
/// in CSS; unsupported functions are ignored. A filter that changes nothing, or
/// malformed input, yields `None`.
pub fn parse_backdrop_filter(s: &str) -> Option<BackdropFilter> {
    let mut f = BackdropFilter {
        blur: 0.0,
        saturate: 1.0,
    };
    let mut rest = s.trim();
    if rest.eq_ignore_ascii_case("none") {
        return None;
    }
    while !rest.is_empty() {
        let open = rest.find('(')?;
        let close = open + rest[open..].find(')')?;
        let name = rest[..open].trim().to_ascii_lowercase();
        let arg = rest[open + 1..close].trim();
        match name.as_str() {
            "blur" => {
                let b = if arg.is_empty() {
                    0.0
                } else {
                    crate::value::parse_length_px(arg)? as f32
                };
                // Two Gaussians in a row are one with the variances added.
                f.blur = (f.blur * f.blur + b.max(0.0).powi(2)).sqrt();
            }
            "saturate" => {
                let v = if arg.is_empty() {
                    1.0
                } else if let Some(p) = arg.strip_suffix('%') {
                    p.trim().parse::<f32>().ok()? / 100.0
                } else {
                    arg.parse::<f32>().ok()?
                };
                f.saturate *= v.max(0.0);
            }
            "" => return None,
            _ => {}
        }
        rest = rest[close + 1..].trim_start();
    }
    (f.blur > 0.0 || (f.saturate - 1.0).abs() > 1e-3).then_some(f)
}

// ───────────────────────────── text ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextAlign {
    Start,
    Center,
    End,
    Justify,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextCase {
    None,
    Upper,
    Lower,
    Capitalize,
}

/// Inherited text properties.
#[derive(Clone, Debug, PartialEq)]
pub struct TextStyle {
    pub color: Color,
    pub family: String,
    pub size: f32,
    pub weight: u16,
    pub italic: bool,
    pub letter_spacing: f32,
    /// Absolute line height in DIPs.
    pub line_height: Option<f32>,
    pub align: TextAlign,
    pub case: TextCase,
    pub wrap: bool,
    pub ellipsis: bool,
    /// `textShadow` (inherited, like CSS): blurred shadow drawn under the text.
    pub shadow: Option<Shadow>,
    /// `textDecoration` / `textDecorationLine`: `underline`, `line-through`.
    /// Applies to nested text too, as decorations do in CSS.
    pub underline: bool,
    pub strike: bool,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            color: Color::WHITE,
            family: "Segoe UI Variable Text".into(),
            size: 14.0,
            weight: 400,
            italic: false,
            letter_spacing: 0.0,
            line_height: None,
            align: TextAlign::Start,
            case: TextCase::None,
            wrap: true,
            ellipsis: false,
            shadow: None,
            underline: false,
            strike: false,
        }
    }
}

/// `(underline, line-through)` from a CSS `text-decoration` value.
pub fn parse_decoration(s: &str) -> (bool, bool) {
    let words: Vec<&str> = s.split_whitespace().collect();
    (
        words.contains(&"underline"),
        words.contains(&"line-through"),
    )
}

fn parse_weight(v: &PropValue) -> Option<u16> {
    match v {
        PropValue::Number(n) => Some(*n as u16),
        PropValue::String(s) => match s.as_str() {
            "normal" => Some(400),
            "bold" => Some(700),
            "lighter" => Some(300),
            "bolder" => Some(800),
            s => s.parse().ok(),
        },
        _ => None,
    }
}

/// First family of a CSS font-family list, without quotes.
pub fn first_family(s: &str) -> String {
    s.split(',')
        .next()
        .unwrap_or(s)
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .to_string()
}

impl TextStyle {
    /// The text style of a node given its parent's style.
    pub fn inherit(parent: &TextStyle, src: &dyn StyleSource) -> TextStyle {
        let mut t = parent.clone();
        if let Some(c) = src
            .string("color")
            .and_then(|s| parse_color(&s, parent.color))
        {
            t.color = c;
        }
        if let Some(f) = src.string("fontFamily") {
            t.family = first_family(&f);
        }
        if let Some(sz) = src.num("fontSize") {
            // An inherited relative line height must follow the new font size.
            if let (Some(lh), true) = (parent.line_height, src.style("lineHeight").is_none()) {
                t.line_height = Some(lh / parent.size * sz);
            }
            t.size = sz;
        }
        if let Some(w) = src.style("fontWeight").as_ref().and_then(parse_weight) {
            t.weight = w;
        }
        if let Some(s) = src.string("fontStyle") {
            t.italic = s == "italic" || s == "oblique";
        }
        if let Some(sh) = src.string("textShadow") {
            t.shadow = if sh.trim() == "none" {
                None
            } else {
                parse_shadow(&sh, t.color)
            };
        }
        if let Some(ls) = src.style("letterSpacing") {
            t.letter_spacing = match &ls {
                PropValue::String(s) if s.ends_with("em") => {
                    s.trim_end_matches("em").parse::<f32>().unwrap_or(0.0) * t.size
                }
                other => other.as_f32().unwrap_or(0.0),
            };
        }
        if let Some(lh) = src.style("lineHeight") {
            t.line_height = match &lh {
                // unitless numbers multiply the font size (CSS semantics)
                PropValue::Number(n) if *n <= 4.0 => Some(*n as f32 * t.size),
                PropValue::String(s) if s.ends_with("px") => {
                    crate::value::parse_length_px(s).map(|v| v as f32)
                }
                PropValue::String(s) if s.ends_with("rem") => s
                    .trim_end_matches("rem")
                    .parse::<f32>()
                    .ok()
                    .map(|v| v * 16.0),
                PropValue::String(s) => s.parse::<f32>().ok().map(|v| v * t.size),
                other => other.as_f32(),
            };
        }
        if let Some(a) = src.string("textAlign") {
            t.align = match a.as_str() {
                "center" => TextAlign::Center,
                "right" | "end" => TextAlign::End,
                "justify" => TextAlign::Justify,
                _ => TextAlign::Start,
            };
        }
        if let Some(c) = src.string("textTransform") {
            t.case = match c.as_str() {
                "uppercase" => TextCase::Upper,
                "lowercase" => TextCase::Lower,
                "capitalize" => TextCase::Capitalize,
                _ => TextCase::None,
            };
        }
        if let Some(w) = src.string("whiteSpace") {
            t.wrap = !(w == "nowrap" || w == "pre");
        }
        if let Some(d) = src
            .string("textDecorationLine")
            .or_else(|| src.string("textDecoration"))
        {
            let (underline, strike) = parse_decoration(&d);
            t.underline = underline;
            t.strike = strike;
        }
        if let Some(o) = src.string("textOverflow") {
            t.ellipsis = o == "ellipsis";
        }
        t
    }

    pub fn apply_case(&self, s: &str) -> String {
        match self.case {
            TextCase::None => s.to_string(),
            TextCase::Upper => s.to_uppercase(),
            TextCase::Lower => s.to_lowercase(),
            TextCase::Capitalize => {
                let mut out = String::with_capacity(s.len());
                let mut start = true;
                for c in s.chars() {
                    if start && c.is_alphabetic() {
                        out.extend(c.to_uppercase());
                        start = false;
                    } else {
                        if c.is_whitespace() {
                            start = true;
                        }
                        out.push(c);
                    }
                }
                out
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Map(Vec<(&'static str, PropValue)>);
    impl StyleSource for Map {
        fn style(&self, key: &str) -> Option<PropValue> {
            self.0
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn grid_template() {
        assert_eq!(parse_grid_template(&"repeat(3, 1fr)".into()).len(), 3);
        assert_eq!(
            parse_grid_template(&"80px 1fr minmax(40px, 1fr)".into()).len(),
            3
        );
        assert_eq!(parse_grid_template(&PropValue::Number(4.0)).len(), 4);
    }

    #[test]
    fn shorthand_padding() {
        let s = layout_style(&Map(vec![("padding", "8 16".into())]));
        assert_eq!(s.padding.left, length(16.0));
        assert_eq!(s.padding.top, length(8.0));
    }

    #[test]
    fn gradient_and_shadow() {
        let f = parse_fill(
            "linear-gradient(135deg, #3d5afe, rgba(124, 58, 237, 0.5) 80%)",
            Color::WHITE,
        )
        .unwrap();
        match f {
            Fill::Linear { angle_deg, stops } => {
                assert_eq!(angle_deg, 135.0);
                assert_eq!(stops.len(), 2);
                assert!((stops[1].0 - 0.8).abs() < 1e-6);
            }
            _ => panic!(),
        }
        let s = parse_shadow("0 4px 12px rgba(0,0,0,0.3)", Color::WHITE).unwrap();
        assert_eq!((s.dy, s.blur), (4.0, 12.0));
    }

    #[test]
    fn backdrop_filters() {
        let f = |s: &str| parse_backdrop_filter(s).map(|f| (f.blur, f.saturate));
        assert_eq!(f("blur(16px) saturate(1.6)"), Some((16.0, 1.6)));
        assert_eq!(f("saturate(1.6) blur(16px)"), Some((16.0, 1.6)));
        assert_eq!(f("  blur( 24 )  "), Some((24.0, 1.0)));
        assert_eq!(f("saturate(180%)"), Some((0.0, 1.8)));
        assert_eq!(f("BLUR(8px)"), Some((8.0, 1.0)));
        // Repeated functions compose: Gaussians add variances, saturations multiply.
        assert_eq!(
            f("blur(3px) blur(4px) saturate(2) saturate(0.5)"),
            Some((5.0, 1.0))
        );
        // Unsupported functions are skipped.
        assert_eq!(f("brightness(1.2) blur(10px)"), Some((10.0, 1.0)));
        // Nothing to do, or malformed.
        for s in [
            "none",
            "NONE",
            "",
            "blur(0)",
            "saturate(1)",
            "saturate(100%)",
            "blur()",
            "brightness(1.2)",
            "blur(16px",
            "blur(abc)",
            "saturate(x)",
            "16px",
            "(4px)",
        ] {
            assert_eq!(f(s), None, "{s:?}");
        }
        // Negative values clamp to zero (a fully desaturated, unblurred glass).
        assert_eq!(f("blur(-4px) saturate(-1)"), Some((0.0, 0.0)));
        // Not inherited: only the node that declares it gets a backdrop.
        let p = box_paint(
            &Map(vec![("backdropFilter", "blur(12px)".into())]),
            Color::WHITE,
        );
        assert_eq!(p.backdrop.map(|b| b.blur), Some(12.0));
        assert_eq!(box_paint(&Map(vec![]), Color::WHITE).backdrop, None);
    }

    #[test]
    fn transform_inverse() {
        let t = parse_transform("translate(10px, 5px) rotate(30) scale(2)");
        let inv = t.invert().unwrap();
        let (x, y) = t.apply(3.0, -7.0);
        let (bx, by) = inv.apply(x, y);
        assert!((bx - 3.0).abs() < 1e-4 && (by + 7.0).abs() < 1e-4);
        assert!(Transform2D::scale(0.0, 1.0).invert().is_none());
    }

    #[test]
    fn transforms() {
        let t = parse_transform("rotate(90 10 10)");
        let (x, y) = t.apply(20.0, 10.0);
        assert!((x - 10.0).abs() < 1e-4 && (y - 20.0).abs() < 1e-4);
        let t = parse_transform("translate(10px, 0) scale(2)");
        assert_eq!(t.apply(1.0, 1.0), (12.0, 2.0));
    }

    #[test]
    fn text_decoration() {
        assert_eq!(parse_decoration("line-through"), (false, true));
        assert_eq!(parse_decoration("underline line-through"), (true, true));
        assert_eq!(parse_decoration("underline dotted red"), (true, false));
        assert_eq!(parse_decoration("none"), (false, false));
        let done = TextStyle::inherit(
            &TextStyle::default(),
            &Map(vec![("textDecoration", "line-through".into())]),
        );
        assert!(done.strike && !done.underline);
        // Nested text keeps the decoration unless it sets its own.
        let child = TextStyle::inherit(&done, &Map(vec![("fontSize", 20.0.into())]));
        assert!(child.strike);
        let cleared = TextStyle::inherit(&done, &Map(vec![("textDecorationLine", "none".into())]));
        assert!(!cleared.strike);
    }

    #[test]
    fn text_inheritance() {
        let parent = TextStyle {
            size: 10.0,
            line_height: Some(15.0),
            ..Default::default()
        };
        let t = TextStyle::inherit(
            &parent,
            &Map(vec![
                ("fontSize", 20.0.into()),
                ("fontWeight", "bold".into()),
            ]),
        );
        assert_eq!(t.line_height, Some(30.0));
        assert_eq!(t.weight, 700);
        let t = TextStyle::inherit(&parent, &Map(vec![("letterSpacing", "0.1em".into())]));
        assert!((t.letter_spacing - 1.0).abs() < 1e-6);
        assert_eq!(
            TextStyle {
                case: TextCase::Capitalize,
                ..Default::default()
            }
            .apply_case("hi there"),
            "Hi There"
        );
    }
}
