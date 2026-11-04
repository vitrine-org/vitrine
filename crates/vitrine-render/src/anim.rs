//! CSS-like transitions on numeric properties (style values and SVG attributes).

use std::collections::HashMap;

use crate::scene::NodeId;
use crate::style::{Transform2D, TransformFn, compose_transform, parse_transform_list};
use crate::value::parse_duration_ms;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Easing {
    Linear,
    Bezier(f32, f32, f32, f32),
}

impl Easing {
    pub fn parse(s: &str) -> Easing {
        match s {
            "linear" => Easing::Linear,
            "ease-in" => Easing::Bezier(0.42, 0.0, 1.0, 1.0),
            "ease-out" => Easing::Bezier(0.0, 0.0, 0.58, 1.0),
            "ease-in-out" => Easing::Bezier(0.42, 0.0, 0.58, 1.0),
            s if s.starts_with("cubic-bezier(") => {
                let v: Vec<f32> = s
                    .trim_start_matches("cubic-bezier(")
                    .trim_end_matches(')')
                    .split(',')
                    .filter_map(|p| p.trim().parse().ok())
                    .collect();
                if v.len() == 4 {
                    Easing::Bezier(v[0], v[1], v[2], v[3])
                } else {
                    Easing::Linear
                }
            }
            _ => Easing::Bezier(0.25, 0.1, 0.25, 1.0), // "ease"
        }
    }

    pub fn apply(&self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match *self {
            Easing::Linear => t,
            Easing::Bezier(x1, y1, x2, y2) => {
                let bez = |a: f32, b: f32, s: f32| {
                    let u = 1.0 - s;
                    3.0 * u * u * s * a + 3.0 * u * s * s * b + s * s * s
                };
                // Solve x(s) = t by bisection; monotonic for valid curves.
                let (mut lo, mut hi) = (0.0f32, 1.0f32);
                for _ in 0..24 {
                    let mid = (lo + hi) * 0.5;
                    if bez(x1, x2, mid) < t {
                        lo = mid
                    } else {
                        hi = mid
                    }
                }
                bez(y1, y2, (lo + hi) * 0.5)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TransitionSpec {
    /// camelCase property name or `all`.
    pub prop: String,
    pub duration_ms: f32,
    pub delay_ms: f32,
    pub easing: Easing,
}

/// Parses `"width 0.4s ease, stroke-dasharray 300ms linear 50ms"`.
pub fn parse_transitions(s: &str) -> Vec<TransitionSpec> {
    s.split(',')
        .filter_map(|part| {
            let mut toks = part.split_whitespace();
            let prop = crate::scene::camel_case(toks.next()?);
            let mut durations = Vec::new();
            let mut easing = Easing::parse("ease");
            for t in toks {
                if let Some(ms) = parse_duration_ms(t).filter(|_| t.ends_with('s')) {
                    durations.push(ms);
                } else {
                    easing = Easing::parse(t);
                }
            }
            Some(TransitionSpec {
                prop,
                duration_ms: durations.first().copied().unwrap_or(0.0),
                delay_ms: durations.get(1).copied().unwrap_or(0.0),
                easing,
            })
        })
        .filter(|t| t.duration_ms > 0.0)
        .collect()
}

#[derive(Clone, Debug)]
struct Anim {
    from: Vec<f32>,
    to: Vec<f32>,
    start: f64,
    duration: f32,
    delay: f32,
    easing: Easing,
}

impl Anim {
    fn end(&self) -> f64 {
        self.start + (self.delay + self.duration) as f64
    }

    fn sample(&self, now: f64) -> Vec<f32> {
        let t = ((now - self.start - self.delay as f64) / self.duration as f64) as f32;
        let k = self.easing.apply(t);
        self.from
            .iter()
            .zip(&self.to)
            .map(|(a, b)| a + (b - a) * k)
            .collect()
    }
}

/// Running transitions keyed by node and property (`s:<style key>` or `a:<attr>`).
#[derive(Default, Debug)]
pub struct Animations {
    map: HashMap<(NodeId, String), Anim>,
}

impl Animations {
    pub fn start(
        &mut self,
        id: NodeId,
        key: String,
        from: Vec<f32>,
        to: Vec<f32>,
        spec: &TransitionSpec,
        now: f64,
    ) {
        if from.len() != to.len() || from.is_empty() || from == to {
            self.map.remove(&(id, key));
            return;
        }
        self.map.insert(
            (id, key),
            Anim {
                from,
                to,
                start: now,
                duration: spec.duration_ms,
                delay: spec.delay_ms,
                easing: spec.easing,
            },
        );
    }

    /// Current value of a running transition, if any.
    pub fn value(&self, id: NodeId, key: &str, now: f64) -> Option<Vec<f32>> {
        let a = self.map.get(&(id, key.to_string()))?;
        (now < a.end()).then(|| a.sample(now))
    }

    pub fn is_active(&self, now: f64) -> bool {
        self.map.values().any(|a| now < a.end())
    }

    /// Whether a running transition affects layout (style keys other than paint-only ones).
    pub fn affects_layout(&self, now: f64) -> bool {
        const PAINT_ONLY: &[&str] = &["s:opacity", "s:borderRadius", "s:transform"];
        self.map.iter().any(|((_, k), a)| {
            now < a.end() && k.starts_with("s:") && !PAINT_ONLY.contains(&k.as_str())
        })
    }

    pub fn gc(&mut self, now: f64) {
        self.map.retain(|_, a| now < a.end());
    }

    pub fn remove_node(&mut self, id: NodeId) {
        self.map.retain(|(n, _), _| *n != id);
    }
}

// ── transform interpolation ───────────────────────────────────────────
//
// A transform transition is a flat list of floats so it runs on the same
// machinery as numbers: each function is `[tag, params…]`. Both ends use the
// same tags, so the tags interpolate to themselves.

const T_TRANSLATE: f32 = 0.0;
const T_SCALE: f32 = 1.0;
const T_ROTATE: f32 = 2.0;
/// A decomposed matrix: translate (2), scale (2), angle, residual 2×2 (4).
const T_DECOMPOSED: f32 = 3.0;

/// A 2D matrix split as in CSS Transforms 1 ("Decomposing a 2D matrix"):
/// translate · rotate(angle) · residual · scale.
fn decompose(m: Transform2D) -> [f32; 9] {
    let [a, b, c, d, e, f] = m.0;
    let (mut sx, mut sy) = ((a * a + b * b).sqrt(), (c * c + d * d).sqrt());
    if a * d - b * c < 0.0 {
        // One axis is flipped: flip the one with the smaller unit dot product.
        if a < d {
            sx = -sx;
        } else {
            sy = -sy;
        }
    }
    let (u0, u1) = (
        if sx != 0.0 {
            (a / sx, b / sx)
        } else {
            (1.0, 0.0)
        },
        if sy != 0.0 {
            (c / sy, d / sy)
        } else {
            (0.0, 1.0)
        },
    );
    let angle = u0.1.atan2(u0.0);
    let (sn, cs) = angle.sin_cos();
    // Rotate both axes by -angle; what is left is the skew.
    let r0 = (cs * u0.0 + sn * u0.1, -sn * u0.0 + cs * u0.1);
    let r1 = (cs * u1.0 + sn * u1.1, -sn * u1.0 + cs * u1.1);
    [e, f, sx, sy, angle.to_degrees(), r0.0, r0.1, r1.0, r1.1]
}

fn recompose(v: &[f32]) -> Transform2D {
    let residual = Transform2D([v[5], v[6], v[7], v[8], 0.0, 0.0]);
    Transform2D::scale(v[2], v[3])
        .then(residual)
        .then(Transform2D::rotate_deg(v[4]))
        .then(Transform2D::translate(v[0], v[1]))
}

/// Makes two decompositions interpolate the short way round (CSS Transforms 1).
fn align_decomposed(a: &mut [f32; 9], b: &mut [f32; 9]) {
    if (a[2] < 0.0 && b[3] < 0.0) || (a[3] < 0.0 && b[2] < 0.0) {
        a[2] = -a[2];
        a[3] = -a[3];
        a[4] += if a[4] < 0.0 { 180.0 } else { -180.0 };
    }
    if a[4] == 0.0 {
        a[4] = 360.0;
    }
    if b[4] == 0.0 {
        b[4] = 360.0;
    }
    if (a[4] - b[4]).abs() > 180.0 {
        if a[4] > b[4] {
            a[4] -= 360.0;
        } else {
            b[4] -= 360.0;
        }
    }
}

fn same_primitive(a: &TransformFn, b: &TransformFn) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b) && !matches!(a, TransformFn::Matrix(_))
}

fn encode_fn(f: &TransformFn, out: &mut Vec<f32>) {
    match *f {
        TransformFn::Translate(x, y) => out.extend([T_TRANSLATE, x, y]),
        TransformFn::Scale(x, y) => out.extend([T_SCALE, x, y]),
        TransformFn::Rotate(deg, cx, cy) => out.extend([T_ROTATE, deg, cx, cy]),
        TransformFn::Matrix(m) => {
            out.push(T_DECOMPOSED);
            out.extend(decompose(m));
        }
    }
}

/// The two ends of a transform transition, following CSS: lists of the same
/// primitives interpolate function by function (the shorter one padded with
/// identities, `none` being the empty list); from the first mismatch on, the
/// rest of each list interpolates as a decomposed matrix.
pub fn transform_pair(from: &str, to: &str) -> (Vec<f32>, Vec<f32>) {
    let mut a = parse_transform_list(from);
    let mut b = parse_transform_list(to);
    while a.len() < b.len() {
        a.push(b[a.len()].identity());
    }
    while b.len() < a.len() {
        b.push(a[b.len()].identity());
    }
    let split = a
        .iter()
        .zip(&b)
        .position(|(x, y)| !same_primitive(x, y))
        .unwrap_or(a.len());
    let (mut va, mut vb) = (Vec::new(), Vec::new());
    for (x, y) in a[..split].iter().zip(&b[..split]) {
        encode_fn(x, &mut va);
        encode_fn(y, &mut vb);
    }
    if split < a.len() {
        let mut da = decompose(compose_transform(&a[split..]));
        let mut db = decompose(compose_transform(&b[split..]));
        align_decomposed(&mut da, &mut db);
        va.push(T_DECOMPOSED);
        va.extend(da);
        vb.push(T_DECOMPOSED);
        vb.extend(db);
    }
    (va, vb)
}

/// The CSS text of an interpolated transform (see [`transform_pair`]).
pub fn transform_css(v: &[f32]) -> String {
    let mut parts = Vec::new();
    let mut i = 0;
    while i < v.len() {
        let rest = &v[i + 1..];
        let (text, n) = match v[i] {
            t if t == T_TRANSLATE && rest.len() >= 2 => {
                (format!("translate({}, {})", rest[0], rest[1]), 2)
            }
            t if t == T_SCALE && rest.len() >= 2 => (format!("scale({}, {})", rest[0], rest[1]), 2),
            t if t == T_ROTATE && rest.len() >= 3 => {
                (format!("rotate({} {} {})", rest[0], rest[1], rest[2]), 3)
            }
            t if t == T_DECOMPOSED && rest.len() >= 9 => {
                let [a, b, c, d, e, f] = recompose(rest).0;
                (format!("matrix({a} {b} {c} {d} {e} {f})"), 9)
            }
            _ => break,
        };
        parts.push(text);
        i += 1 + n;
    }
    if parts.is_empty() {
        "none".into()
    } else {
        parts.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::parse_transform;

    fn close(a: Transform2D, b: Transform2D) -> bool {
        a.0.iter().zip(b.0).all(|(x, y)| (x - y).abs() < 1e-3)
    }

    fn mid(from: &str, to: &str, k: f32) -> Transform2D {
        let (a, b) = transform_pair(from, to);
        assert_eq!(a.len(), b.len());
        let v: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x + (y - x) * k).collect();
        crate::style::parse_transform(&transform_css(&v))
    }

    #[test]
    fn transform_component_wise() {
        // The Essentials to-do check mark.
        assert!(close(
            mid("scale(0.6)", "scale(1)", 0.5),
            Transform2D::scale(0.8, 0.8)
        ));
        assert!(close(
            mid("translateX(0px)", "translate(10px, 20px)", 0.5),
            Transform2D::translate(5.0, 10.0)
        ));
        // Same primitives keep the long way round, as in CSS.
        assert!(close(
            mid("rotate(0deg)", "rotate(360deg)", 0.25),
            Transform2D::rotate_deg(90.0)
        ));
        // `none` is padded with identities.
        assert!(close(
            mid("", "scale(2) rotate(90deg)", 0.5),
            parse_transform("scale(1.5) rotate(45deg)")
        ));
        assert!(close(
            mid("translateY(8px)", "none", 0.25),
            Transform2D::translate(0.0, 6.0)
        ));
        // Ends are exact.
        assert!(close(
            mid("scale(0.94)", "scale(1)", 1.0),
            Transform2D::IDENTITY
        ));
    }

    #[test]
    fn transform_matrix_fallback() {
        // Different primitives: interpolated as decomposed matrices.
        assert!(close(
            mid("scale(2)", "rotate(90deg)", 0.0),
            Transform2D::scale(2.0, 2.0)
        ));
        assert!(close(
            mid("scale(2)", "rotate(90deg)", 1.0),
            Transform2D::rotate_deg(90.0)
        ));
        let half = mid("scale(2)", "rotate(90deg)", 0.5);
        assert!(
            close(half, parse_transform("rotate(45deg) scale(1.5)")),
            "{half:?}"
        );
        // Matrices decompose and recompose (with a flip).
        let m = parse_transform("translate(3, 4) rotate(30) scale(-2, 1.5)");
        assert!(close(recompose(&decompose(m)), m));
        let m = "matrix(1 0 0 1 10 0)";
        assert!(close(
            mid(m, "matrix(1 0 0 1 20 0)", 0.5),
            Transform2D::translate(15.0, 0.0)
        ));
        // A shared prefix stays component-wise; the tail goes through a matrix.
        let (a, _) = transform_pair("translate(4px) scale(2)", "translate(8px) rotate(10deg)");
        assert_eq!(a[0], T_TRANSLATE);
        assert_eq!(a[3], T_DECOMPOSED);
        assert_eq!(transform_css(&[]), "none");
    }

    #[test]
    fn parse_and_sample() {
        let specs = parse_transitions("stroke-dasharray 0.4s ease, opacity 200ms linear 100ms");
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].prop, "strokeDasharray");
        assert_eq!(specs[1].delay_ms, 100.0);
        let mut a = Animations::default();
        a.start(1, "a:x".into(), vec![0.0], vec![100.0], &specs[1], 0.0);
        assert_eq!(a.value(1, "a:x", 50.0), Some(vec![0.0]));
        assert_eq!(a.value(1, "a:x", 200.0), Some(vec![50.0]));
        assert_eq!(a.value(1, "a:x", 400.0), None);
        assert!((Easing::parse("ease").apply(0.5) - 0.8024).abs() < 0.01);
    }
}
