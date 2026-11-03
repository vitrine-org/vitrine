//! CSS-like color parsing.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const TRANSPARENT: Color = Color::rgba(0.0, 0.0, 0.0, 0.0);
    pub const WHITE: Color = Color::rgba(1.0, 1.0, 1.0, 1.0);
    pub const BLACK: Color = Color::rgba(0.0, 0.0, 0.0, 1.0);

    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    pub fn from_rgb8(r: u8, g: u8, b: u8, a: f32) -> Self {
        Self::rgba(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a)
    }

    pub fn with_alpha_mul(self, k: f32) -> Self {
        Self {
            a: self.a * k,
            ..self
        }
    }

    pub fn lerp(self, other: Color, t: f32) -> Color {
        let l = |a: f32, b: f32| a + (b - a) * t;
        Color::rgba(
            l(self.r, other.r),
            l(self.g, other.g),
            l(self.b, other.b),
            l(self.a, other.a),
        )
    }

    /// Parses `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, `rgb()`, `rgba()`, `hsl()`,
    /// `hsla()`, `transparent` and a set of common named colors.
    /// `currentColor` is resolved by the caller and returns `None` here.
    pub fn parse(s: &str) -> Option<Color> {
        let s = s.trim();
        if let Some(hex) = s.strip_prefix('#') {
            return parse_hex(hex);
        }
        let lower = s.to_ascii_lowercase();
        if let Some(args) = func_args(&lower, "rgba").or_else(|| func_args(&lower, "rgb")) {
            let parts = split_args(args);
            if parts.len() < 3 {
                return None;
            }
            let channel = |p: &str| -> Option<f32> {
                if let Some(pct) = p.strip_suffix('%') {
                    pct.parse::<f32>().ok().map(|v| v / 100.0)
                } else {
                    p.parse::<f32>().ok().map(|v| v / 255.0)
                }
            };
            let a = parts.get(3).map(|p| parse_alpha(p)).unwrap_or(Some(1.0))?;
            return Some(Color::rgba(
                channel(parts[0])?,
                channel(parts[1])?,
                channel(parts[2])?,
                a,
            ));
        }
        if let Some(args) = func_args(&lower, "hsla").or_else(|| func_args(&lower, "hsl")) {
            let parts = split_args(args);
            if parts.len() < 3 {
                return None;
            }
            let h = parts[0].trim_end_matches("deg").parse::<f32>().ok()?;
            let sat = parts[1].trim_end_matches('%').parse::<f32>().ok()? / 100.0;
            let l = parts[2].trim_end_matches('%').parse::<f32>().ok()? / 100.0;
            let a = parts.get(3).map(|p| parse_alpha(p)).unwrap_or(Some(1.0))?;
            let (r, g, b) = hsl_to_rgb(h, sat, l);
            return Some(Color::rgba(r, g, b, a));
        }
        named(&lower)
    }
}

fn parse_alpha(p: &str) -> Option<f32> {
    if let Some(pct) = p.strip_suffix('%') {
        pct.parse::<f32>().ok().map(|v| v / 100.0)
    } else {
        p.parse::<f32>().ok()
    }
}

fn func_args<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let rest = s.strip_prefix(name)?.trim_start();
    rest.strip_prefix('(')?.strip_suffix(')')
}

/// Splits `1, 2, 3` or `1 2 3 / 0.5`.
fn split_args(args: &str) -> Vec<&str> {
    args.split(|c: char| c == ',' || c == '/' || c.is_whitespace())
        .filter(|p| !p.is_empty())
        .collect()
}

fn parse_hex(hex: &str) -> Option<Color> {
    let digit = |i: usize| u8::from_str_radix(&hex[i..i + 1], 16).ok().map(|v| v * 17);
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    match hex.len() {
        3 => Some(Color::from_rgb8(digit(0)?, digit(1)?, digit(2)?, 1.0)),
        4 => Some(Color::from_rgb8(
            digit(0)?,
            digit(1)?,
            digit(2)?,
            digit(3)? as f32 / 255.0,
        )),
        6 => Some(Color::from_rgb8(byte(0)?, byte(2)?, byte(4)?, 1.0)),
        8 => Some(Color::from_rgb8(
            byte(0)?,
            byte(2)?,
            byte(4)?,
            byte(6)? as f32 / 255.0,
        )),
        _ => None,
    }
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let h = ((h % 360.0) + 360.0) % 360.0 / 360.0;
    if s == 0.0 {
        return (l, l, l);
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let hue = |mut t: f32| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    (hue(h + 1.0 / 3.0), hue(h), hue(h - 1.0 / 3.0))
}

fn named(s: &str) -> Option<Color> {
    let rgb = |v: u32| {
        Some(Color::from_rgb8(
            (v >> 16) as u8,
            (v >> 8) as u8,
            v as u8,
            1.0,
        ))
    };
    match s {
        "transparent" => Some(Color::TRANSPARENT),
        "black" => rgb(0x000000),
        "white" => rgb(0xffffff),
        "red" => rgb(0xff0000),
        "green" => rgb(0x008000),
        "lime" => rgb(0x00ff00),
        "blue" => rgb(0x0000ff),
        "yellow" => rgb(0xffff00),
        "orange" => rgb(0xffa500),
        "purple" => rgb(0x800080),
        "gray" | "grey" => rgb(0x808080),
        "silver" => rgb(0xc0c0c0),
        "cyan" | "aqua" => rgb(0x00ffff),
        "magenta" | "fuchsia" => rgb(0xff00ff),
        "pink" => rgb(0xffc0cb),
        "navy" => rgb(0x000080),
        "teal" => rgb(0x008080),
        "maroon" => rgb(0x800000),
        "olive" => rgb(0x808000),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        assert_eq!(Color::parse("#fff"), Some(Color::WHITE));
        assert_eq!(Color::parse("#40C4FF").unwrap().a, 1.0);
        let c = Color::parse("rgba(40, 42, 54, 0.78)").unwrap();
        assert!((c.a - 0.78).abs() < 1e-6);
        assert!((c.r - 40.0 / 255.0).abs() < 1e-6);
        assert_eq!(Color::parse("transparent"), Some(Color::TRANSPARENT));
        let h = Color::parse("hsl(0, 100%, 50%)").unwrap();
        assert!((h.r - 1.0).abs() < 1e-5 && h.g.abs() < 1e-5);
        assert!(Color::parse("currentColor").is_none());
        assert_eq!(Color::parse("#00000080").unwrap().a, 128.0 / 255.0);
    }
}
