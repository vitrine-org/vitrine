//! CSS-like transitions on numeric properties (style values and SVG attributes).

use std::collections::HashMap;

use crate::scene::NodeId;
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

#[cfg(test)]
mod tests {
    use super::*;

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
