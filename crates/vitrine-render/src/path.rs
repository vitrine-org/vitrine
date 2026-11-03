//! SVG path data parser producing absolute drawing commands.

#[derive(Clone, Debug, PartialEq)]
pub enum PathCmd {
    Move(f32, f32),
    Line(f32, f32),
    Cubic(f32, f32, f32, f32, f32, f32),
    Quad(f32, f32, f32, f32),
    /// rx, ry, x-axis rotation (deg), large arc, sweep (clockwise in y-down), end x, end y
    Arc(f32, f32, f32, bool, bool, f32, f32),
    Close,
}

struct Lexer<'a> {
    s: &'a [u8],
    i: usize,
}

impl Lexer<'_> {
    fn skip(&mut self) {
        while self.i < self.s.len()
            && (self.s[self.i].is_ascii_whitespace() || self.s[self.i] == b',')
        {
            self.i += 1;
        }
    }

    fn command(&mut self) -> Option<u8> {
        self.skip();
        let c = *self.s.get(self.i)?;
        if c.is_ascii_alphabetic() {
            self.i += 1;
            Some(c)
        } else {
            None
        }
    }

    fn at_number(&mut self) -> bool {
        self.skip();
        matches!(self.s.get(self.i), Some(c) if c.is_ascii_digit() || *c == b'-' || *c == b'+' || *c == b'.')
    }

    fn number(&mut self) -> Option<f32> {
        self.skip();
        let start = self.i;
        let mut seen_dot = false;
        let mut seen_e = false;
        if matches!(self.s.get(self.i), Some(b'-') | Some(b'+')) {
            self.i += 1;
        }
        while let Some(&c) = self.s.get(self.i) {
            match c {
                b'0'..=b'9' => self.i += 1,
                b'.' if !seen_dot && !seen_e => {
                    seen_dot = true;
                    self.i += 1
                }
                b'e' | b'E' if !seen_e => {
                    seen_e = true;
                    self.i += 1;
                    if matches!(self.s.get(self.i), Some(b'-') | Some(b'+')) {
                        self.i += 1;
                    }
                }
                _ => break,
            }
        }
        std::str::from_utf8(&self.s[start..self.i])
            .ok()?
            .parse()
            .ok()
    }

    /// Arc flags may be written without separators ("a1 1 0 011 1").
    fn flag(&mut self) -> Option<bool> {
        self.skip();
        let c = *self.s.get(self.i)?;
        self.i += 1;
        match c {
            b'0' => Some(false),
            b'1' => Some(true),
            _ => None,
        }
    }
}

/// Parses SVG path data. Invalid trailing data stops parsing (like browsers).
pub fn parse_path(d: &str) -> Vec<PathCmd> {
    let mut lx = Lexer {
        s: d.as_bytes(),
        i: 0,
    };
    let mut out = Vec::new();
    let (mut cx, mut cy) = (0.0f32, 0.0f32);
    let (mut sx, mut sy) = (0.0f32, 0.0f32);
    // last control point for S/T reflection
    let mut last_cubic: Option<(f32, f32)> = None;
    let mut last_quad: Option<(f32, f32)> = None;
    let mut cmd = match lx.command() {
        Some(c) => c,
        None => return out,
    };
    loop {
        let rel = cmd.is_ascii_lowercase();
        let (ox, oy) = if rel { (cx, cy) } else { (0.0, 0.0) };
        let upper = cmd.to_ascii_uppercase();
        let ok = (|| -> Option<()> {
            match upper {
                b'M' => {
                    let (x, y) = (lx.number()? + ox, lx.number()? + oy);
                    out.push(PathCmd::Move(x, y));
                    (cx, cy, sx, sy) = (x, y, x, y);
                    // subsequent pairs are implicit line-tos
                    cmd = if rel { b'l' } else { b'L' };
                    last_cubic = None;
                    last_quad = None;
                }
                b'L' => {
                    let (x, y) = (lx.number()? + ox, lx.number()? + oy);
                    out.push(PathCmd::Line(x, y));
                    (cx, cy) = (x, y);
                    last_cubic = None;
                    last_quad = None;
                }
                b'H' => {
                    let x = lx.number()? + ox;
                    out.push(PathCmd::Line(x, cy));
                    cx = x;
                    last_cubic = None;
                    last_quad = None;
                }
                b'V' => {
                    let y = lx.number()? + if rel { cy } else { 0.0 };
                    out.push(PathCmd::Line(cx, y));
                    cy = y;
                    last_cubic = None;
                    last_quad = None;
                }
                b'C' => {
                    let v = [
                        lx.number()?,
                        lx.number()?,
                        lx.number()?,
                        lx.number()?,
                        lx.number()?,
                        lx.number()?,
                    ];
                    let (x1, y1, x2, y2, x, y) = (
                        v[0] + ox,
                        v[1] + oy,
                        v[2] + ox,
                        v[3] + oy,
                        v[4] + ox,
                        v[5] + oy,
                    );
                    out.push(PathCmd::Cubic(x1, y1, x2, y2, x, y));
                    last_cubic = Some((x2, y2));
                    last_quad = None;
                    (cx, cy) = (x, y);
                }
                b'S' => {
                    let v = [lx.number()?, lx.number()?, lx.number()?, lx.number()?];
                    let (x1, y1) = match last_cubic {
                        Some((px, py)) => (2.0 * cx - px, 2.0 * cy - py),
                        None => (cx, cy),
                    };
                    let (x2, y2, x, y) = (v[0] + ox, v[1] + oy, v[2] + ox, v[3] + oy);
                    out.push(PathCmd::Cubic(x1, y1, x2, y2, x, y));
                    last_cubic = Some((x2, y2));
                    last_quad = None;
                    (cx, cy) = (x, y);
                }
                b'Q' => {
                    let v = [lx.number()?, lx.number()?, lx.number()?, lx.number()?];
                    let (x1, y1, x, y) = (v[0] + ox, v[1] + oy, v[2] + ox, v[3] + oy);
                    out.push(PathCmd::Quad(x1, y1, x, y));
                    last_quad = Some((x1, y1));
                    last_cubic = None;
                    (cx, cy) = (x, y);
                }
                b'T' => {
                    let (x, y) = (lx.number()? + ox, lx.number()? + oy);
                    let (x1, y1) = match last_quad {
                        Some((px, py)) => (2.0 * cx - px, 2.0 * cy - py),
                        None => (cx, cy),
                    };
                    out.push(PathCmd::Quad(x1, y1, x, y));
                    last_quad = Some((x1, y1));
                    last_cubic = None;
                    (cx, cy) = (x, y);
                }
                b'A' => {
                    let (rx, ry, rot) = (lx.number()?, lx.number()?, lx.number()?);
                    let (large, sweep) = (lx.flag()?, lx.flag()?);
                    let (x, y) = (lx.number()? + ox, lx.number()? + oy);
                    out.push(PathCmd::Arc(rx.abs(), ry.abs(), rot, large, sweep, x, y));
                    (cx, cy) = (x, y);
                    last_cubic = None;
                    last_quad = None;
                }
                b'Z' => {
                    out.push(PathCmd::Close);
                    (cx, cy) = (sx, sy);
                    last_cubic = None;
                    last_quad = None;
                }
                _ => return None,
            }
            Some(())
        })();
        if ok.is_none() {
            break;
        }
        if upper != b'Z' && lx.at_number() {
            continue; // implicit repetition of the same command
        }
        match lx.command() {
            Some(c) => cmd = c,
            None => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_forms() {
        let p = parse_path("M10 10h20v-5l5,5 L0 0z");
        assert_eq!(
            p,
            vec![
                PathCmd::Move(10.0, 10.0),
                PathCmd::Line(30.0, 10.0),
                PathCmd::Line(30.0, 5.0),
                PathCmd::Line(35.0, 10.0),
                PathCmd::Line(0.0, 0.0),
                PathCmd::Close
            ]
        );
        let p = parse_path("m1 1 2 2");
        assert_eq!(p, vec![PathCmd::Move(1.0, 1.0), PathCmd::Line(3.0, 3.0)]);
        let p = parse_path("M0 0a5 5 0 011 1");
        assert_eq!(p[1], PathCmd::Arc(5.0, 5.0, 0.0, false, true, 1.0, 1.0));
        let p = parse_path("M0 0C1 1 2 2 3 3S5 5 6 6");
        assert_eq!(p[2], PathCmd::Cubic(4.0, 4.0, 5.0, 5.0, 6.0, 6.0));
        assert_eq!(parse_path("M-1.5.5"), vec![PathCmd::Move(-1.5, 0.5)]);
    }
}
