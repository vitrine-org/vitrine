//! Dynamically typed property values coming from widget scripts.

use std::fmt;

#[derive(Clone, Debug, PartialEq, Default)]
pub enum PropValue {
    #[default]
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    List(Vec<PropValue>),
    Map(Vec<(String, PropValue)>),
}

impl PropValue {
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(*n),
            Self::String(s) => parse_length_px(s),
            Self::Bool(b) => Some(*b as u8 as f64),
            _ => None,
        }
    }

    pub fn as_f32(&self) -> Option<f32> {
        self.as_f64().map(|v| v as f32)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> bool {
        match self {
            Self::Null => false,
            Self::Bool(b) => *b,
            Self::Number(n) => *n != 0.0,
            Self::String(s) => !s.is_empty() && s != "false",
            _ => true,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// Numbers from a number, a list of numbers, or a space/comma separated string.
    pub fn as_numbers(&self) -> Vec<f32> {
        match self {
            Self::Number(n) => vec![*n as f32],
            Self::List(items) => items.iter().filter_map(|v| v.as_f32()).collect(),
            Self::String(s) => s
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|t| !t.is_empty())
                .filter_map(parse_length_px)
                .map(|v| v as f32)
                .collect(),
            _ => Vec::new(),
        }
    }

    pub fn get(&self, key: &str) -> Option<&PropValue> {
        match self {
            Self::Map(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

impl fmt::Display for PropValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => write!(f, "null"),
            Self::Bool(b) => write!(f, "{b}"),
            Self::Number(n) => write!(f, "{n}"),
            Self::String(s) => write!(f, "{s}"),
            Self::List(items) => {
                let parts: Vec<String> = items.iter().map(|v| v.to_string()).collect();
                write!(f, "{}", parts.join(" "))
            }
            Self::Map(_) => write!(f, "[object]"),
        }
    }
}

impl From<f64> for PropValue {
    fn from(v: f64) -> Self {
        Self::Number(v)
    }
}

impl From<&str> for PropValue {
    fn from(v: &str) -> Self {
        Self::String(v.to_string())
    }
}

impl From<bool> for PropValue {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}

/// Parses `12`, `12px`, `1.5`, `-3px`. Percentages and other units return `None`.
pub fn parse_length_px(s: &str) -> Option<f64> {
    let s = s.trim();
    let s = s.strip_suffix("px").unwrap_or(s);
    s.parse::<f64>().ok()
}

/// A length that may be absolute, relative to the parent, or automatic.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Length {
    Px(f32),
    Percent(f32),
    Auto,
}

impl Length {
    pub fn parse(v: &PropValue) -> Option<Self> {
        match v {
            PropValue::Number(n) => Some(Self::Px(*n as f32)),
            PropValue::String(s) => {
                let s = s.trim();
                if s == "auto" {
                    Some(Self::Auto)
                } else if let Some(p) = s.strip_suffix('%') {
                    p.trim().parse::<f32>().ok().map(Self::Percent)
                } else {
                    parse_length_px(s).map(|v| Self::Px(v as f32))
                }
            }
            _ => None,
        }
    }
}

/// Parses a duration such as `0.4s`, `200ms` or a bare number of milliseconds.
pub fn parse_duration_ms(s: &str) -> Option<f32> {
    let s = s.trim();
    if let Some(ms) = s.strip_suffix("ms") {
        ms.trim().parse().ok()
    } else if let Some(sec) = s.strip_suffix('s') {
        sec.trim().parse::<f32>().ok().map(|v| v * 1000.0)
    } else {
        s.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths() {
        assert_eq!(
            Length::parse(&PropValue::Number(4.0)),
            Some(Length::Px(4.0))
        );
        assert_eq!(Length::parse(&"50%".into()), Some(Length::Percent(50.0)));
        assert_eq!(Length::parse(&"12px".into()), Some(Length::Px(12.0)));
        assert_eq!(Length::parse(&"auto".into()), Some(Length::Auto));
        assert_eq!(parse_duration_ms("0.4s"), Some(400.0));
        assert_eq!(parse_duration_ms("150ms"), Some(150.0));
    }

    #[test]
    fn numbers() {
        assert_eq!(PropValue::from("60 151").as_numbers(), vec![60.0, 151.0]);
        assert_eq!(PropValue::from("4, 2").as_numbers(), vec![4.0, 2.0]);
    }
}
