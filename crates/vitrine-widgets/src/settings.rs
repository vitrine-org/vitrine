//! Widget settings schema (`settings.json` of a widget).

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::locale::{self, Translations};
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingType {
    Boolean,
    Number,
    String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingOption {
    pub value: Value,
    #[serde(default)]
    pub label: String,
    /// `label` by language tag.
    #[serde(default, skip_serializing_if = "Translations::is_empty")]
    pub labels: Translations,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Condition {
    pub key: String,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingDef {
    #[serde(rename = "type")]
    pub kind: SettingType,
    #[serde(default)]
    pub default: Option<Value>,
    #[serde(default)]
    pub label: String,
    /// `label` by language tag (`{"it": "Secondi"}`).
    #[serde(default, skip_serializing_if = "Translations::is_empty")]
    pub labels: Translations,
    #[serde(default)]
    pub description: String,
    /// `description` by language tag.
    #[serde(default, skip_serializing_if = "Translations::is_empty")]
    pub descriptions: Translations,
    #[serde(default)]
    pub options: Vec<SettingOption>,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub pattern_message: Option<String>,
    /// `patternMessage` by language tag.
    #[serde(default, skip_serializing_if = "Translations::is_empty")]
    pub pattern_messages: Translations,
    #[serde(default)]
    pub visible_when: Option<Condition>,
    #[serde(default)]
    pub disabled_when: Option<Condition>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub step: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub title: String,
    /// `title` by language tag.
    pub titles: Translations,
    /// Settings in declaration order.
    pub settings: Vec<(String, SettingDef)>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SettingsSchema {
    pub sections: Vec<Section>,
}

#[derive(Deserialize)]
struct RawSection {
    #[serde(default)]
    title: String,
    #[serde(default)]
    titles: Translations,
    settings: Map<String, Value>,
}

/// Per-key validation errors: `(key, message)`.
pub type KeyErrors = Vec<(String, String)>;

fn num_value(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 9.0e15 {
        Value::from(n as i64)
    } else {
        serde_json::Number::from_f64(n)
            .map(Value::Number)
            .unwrap_or(Value::Null)
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        _ => a == b,
    }
}

impl SettingDef {
    /// Best-effort conversion to the declared type.
    fn coerce(&self, v: &Value) -> std::result::Result<Value, String> {
        match self.kind {
            SettingType::Boolean => match v {
                Value::Bool(_) => Ok(v.clone()),
                Value::String(s) if s.eq_ignore_ascii_case("true") => Ok(Value::Bool(true)),
                Value::String(s) if s.eq_ignore_ascii_case("false") => Ok(Value::Bool(false)),
                _ => Err("expected a boolean".into()),
            },
            SettingType::Number => {
                let n = match v {
                    Value::Number(n) => n.as_f64(),
                    Value::String(s) => s.trim().parse::<f64>().ok(),
                    _ => None,
                };
                match n {
                    Some(n) if n.is_finite() => Ok(num_value(n)),
                    _ => Err("expected a number".into()),
                }
            }
            SettingType::String => match v {
                Value::String(_) => Ok(v.clone()),
                Value::Number(n) => Ok(Value::String(n.to_string())),
                Value::Bool(b) => Ok(Value::String(b.to_string())),
                _ => Err("expected a string".into()),
            },
        }
    }

    fn fallback(&self) -> Value {
        match self.kind {
            SettingType::Boolean => Value::Bool(false),
            SettingType::Number => num_value(self.min.unwrap_or(0.0)),
            SettingType::String => Value::String(String::new()),
        }
    }

    /// The default value (declared, or a type-appropriate fallback).
    pub fn default_value(&self) -> Value {
        self.default.clone().unwrap_or_else(|| self.fallback())
    }

    fn clamp(&self, v: Value) -> Value {
        if self.kind != SettingType::Number {
            return v;
        }
        let Some(mut n) = v.as_f64() else { return v };
        if let Some(min) = self.min {
            n = n.max(min);
        }
        if let Some(max) = self.max {
            n = n.min(max);
        }
        num_value(n)
    }

    fn check(&self, v: Value) -> std::result::Result<Value, String> {
        let v = self.coerce(&v)?;
        let v = self.clamp(v);
        if !self.options.is_empty() && !self.options.iter().any(|o| values_equal(&o.value, &v)) {
            return Err("value is not one of the allowed options".into());
        }
        if let (Some(p), Value::String(s)) = (&self.pattern, &v) {
            let re = Regex::new(p).map_err(|e| format!("bad pattern: {e}"))?;
            if !re.is_match(s) {
                return Err(self
                    .pattern_message
                    .clone()
                    .unwrap_or_else(|| format!("value does not match `{p}`")));
            }
        }
        Ok(v)
    }
}

impl SettingsSchema {
    /// Parses a schema: a JSON array of sections.
    pub fn parse(json: &str) -> Result<SettingsSchema> {
        let raw: Vec<RawSection> = serde_json::from_str(json)?;
        let mut sections = Vec::with_capacity(raw.len());
        let mut seen = std::collections::HashSet::new();
        for rs in raw {
            let mut settings = Vec::with_capacity(rs.settings.len());
            for (key, val) in rs.settings {
                if !seen.insert(key.clone()) {
                    return Err(Error::Schema(format!("duplicate setting key `{key}`")));
                }
                let def: SettingDef = serde_json::from_value(val)
                    .map_err(|e| Error::Schema(format!("setting `{key}`: {e}")))?;
                let what = |field: &str| format!("setting `{key}`: {field}");
                locale::validate(&def.labels, &what("labels"))
                    .and_then(|_| locale::validate(&def.descriptions, &what("descriptions")))
                    .and_then(|_| locale::validate(&def.pattern_messages, &what("patternMessages")))
                    .map_err(Error::Schema)?;
                for o in &def.options {
                    locale::validate(&o.labels, &what("option labels")).map_err(Error::Schema)?;
                }
                if let Some(p) = &def.pattern {
                    Regex::new(p)
                        .map_err(|e| Error::Schema(format!("setting `{key}`: bad pattern: {e}")))?;
                }
                if let (Some(a), Some(b)) = (def.min, def.max)
                    && a > b
                {
                    return Err(Error::Schema(format!("setting `{key}`: min > max")));
                }
                if let Some(d) = &def.default {
                    def.coerce(d)
                        .map_err(|e| Error::Schema(format!("setting `{key}`: default: {e}")))?;
                }
                settings.push((key, def));
            }
            locale::validate(&rs.titles, "section titles").map_err(Error::Schema)?;
            sections.push(Section {
                title: rs.title,
                titles: rs.titles,
                settings,
            });
        }
        Ok(SettingsSchema { sections })
    }

    /// Replaces titles, labels, descriptions and pattern messages with their
    /// `lang` variants (closest tag; missing ones keep the schema's own text).
    pub fn localize(&mut self, lang: &str) {
        for section in &mut self.sections {
            locale::apply(&mut section.title, &section.titles, lang);
            for (_, def) in &mut section.settings {
                locale::apply(&mut def.label, &def.labels, lang);
                locale::apply(&mut def.description, &def.descriptions, lang);
                if let Some(v) = locale::pick(&def.pattern_messages, lang) {
                    def.pattern_message = Some(v.to_string());
                }
                for o in &mut def.options {
                    locale::apply(&mut o.label, &o.labels, lang);
                }
            }
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &SettingDef)> {
        self.sections
            .iter()
            .flat_map(|s| s.settings.iter().map(|(k, d)| (k.as_str(), d)))
    }

    pub fn get(&self, key: &str) -> Option<&SettingDef> {
        self.iter().find(|(k, _)| *k == key).map(|(_, d)| d)
    }

    /// All declared defaults, in schema order.
    pub fn defaults(&self) -> Map<String, Value> {
        self.iter()
            .map(|(k, d)| (k.to_string(), d.default_value()))
            .collect()
    }

    /// Fills defaults, drops unknown keys, coerces types, clamps numbers,
    /// rejects values outside `options` and values failing `pattern`.
    /// Returns every per-key error if any value is invalid.
    pub fn normalize(
        &self,
        values: &Map<String, Value>,
    ) -> std::result::Result<Map<String, Value>, KeyErrors> {
        let mut out = Map::new();
        let mut errors = Vec::new();
        for (key, def) in self.iter() {
            let value = match values.get(key) {
                Some(v) if !v.is_null() => v.clone(),
                _ => def.default_value(),
            };
            match def.check(value) {
                Ok(v) => {
                    out.insert(key.to_string(), v);
                }
                Err(e) => errors.push((key.to_string(), e)),
            }
        }
        if errors.is_empty() {
            Ok(out)
        } else {
            Err(errors)
        }
    }

    fn effective(&self, key: &str, values: &Map<String, Value>) -> Option<Value> {
        match values.get(key) {
            Some(v) if !v.is_null() => Some(v.clone()),
            _ => self.get(key).map(SettingDef::default_value),
        }
    }

    fn matches(&self, c: &Condition, values: &Map<String, Value>) -> bool {
        self.effective(&c.key, values)
            .is_some_and(|v| values_equal(&v, &c.value))
    }

    /// False when the setting's `visibleWhen` condition is not met.
    pub fn is_visible(&self, key: &str, values: &Map<String, Value>) -> bool {
        match self.get(key).and_then(|d| d.visible_when.as_ref()) {
            Some(c) => self.matches(c, values),
            None => true,
        }
    }

    /// True when the setting's `disabledWhen` condition is met.
    pub fn is_disabled(&self, key: &str, values: &Map<String, Value>) -> bool {
        match self.get(key).and_then(|d| d.disabled_when.as_ref()) {
            Some(c) => self.matches(c, values),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SCHEMA: &str = r##"[
      { "title": "General", "settings": {
        "zeta": { "type": "boolean", "default": true, "label": "Z" },
        "size": { "type": "number", "default": 10, "min": 5, "max": 20, "step": 1 },
        "font": { "type": "string", "default": "a", "options": [
          {"value": "a", "label": "A"}, {"value": "b", "label": "B"} ] }
      }},
      { "title": "Colors", "settings": {
        "custom": { "type": "boolean", "default": false },
        "color": { "type": "string", "default": "#ffffff", "pattern": "^#[0-9a-fA-F]{6}$",
                   "patternMessage": "Use #RRGGBB",
                   "visibleWhen": {"key": "custom", "value": true},
                   "disabledWhen": {"key": "zeta", "value": false} }
      }}
    ]"##;

    fn schema() -> SettingsSchema {
        SettingsSchema::parse(SCHEMA).unwrap()
    }

    #[test]
    fn label_maps_localize_and_are_optional() {
        let json = r##"[
          { "title": "Look", "titles": { "it": "Aspetto" }, "settings": {
            "seconds": { "type": "boolean", "label": "Seconds",
                         "labels": { "it": "Secondi", "zh-CN": "秒" },
                         "description": "Show seconds", "descriptions": { "de": "Sekunden anzeigen" } },
            "unit": { "type": "string", "default": "c", "label": "Unit", "options": [
              { "value": "c", "label": "Celsius", "labels": { "it": "Celsius (°C)" } },
              { "value": "f", "label": "Fahrenheit" } ] },
            "color": { "type": "string", "pattern": "^#", "patternMessage": "Use #RRGGBB",
                       "patternMessages": { "it": "Usa #RRGGBB" } }
          }}
        ]"##;
        let base = SettingsSchema::parse(json).unwrap();
        let mut it = base.clone();
        it.localize("it-IT");
        assert_eq!(it.sections[0].title, "Aspetto");
        let s = &it.sections[0].settings;
        assert_eq!(s[0].1.label, "Secondi");
        assert_eq!(s[0].1.description, "Show seconds");
        assert_eq!(s[1].1.options[0].label, "Celsius (°C)");
        assert_eq!(s[1].1.options[1].label, "Fahrenheit");
        assert_eq!(s[2].1.pattern_message.as_deref(), Some("Usa #RRGGBB"));
        let mut zh = base.clone();
        zh.localize("zh");
        assert_eq!(zh.sections[0].title, "Look");
        assert_eq!(zh.sections[0].settings[0].1.label, "秒");
        let mut de = base.clone();
        de.localize("de");
        assert_eq!(
            de.sections[0].settings[0].1.description,
            "Sekunden anzeigen"
        );
        // The plain schema is unchanged by an unknown language.
        let mut en = base.clone();
        en.localize("en");
        assert_eq!(en, base);

        for bad in [
            r#"[{"settings":{"a":{"type":"string","labels":{"it!":"x"}}}}]"#,
            r#"[{"settings":{"a":{"type":"string","labels":{"it":""}}}}]"#,
            r#"[{"settings":{"a":{"type":"string","labels":"x"}}}]"#,
            r#"[{"titles":{"it":3},"settings":{}}]"#,
            r#"[{"titles":{"??":"x"},"settings":{}}]"#,
            r#"[{"settings":{"a":{"type":"string","options":[{"value":1,"labels":{"x y":"a"}}]}}}]"#,
        ] {
            assert!(SettingsSchema::parse(bad).is_err(), "{bad}");
        }
    }

    fn map(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn defaults_keep_order() {
        let d = schema().defaults();
        let keys: Vec<_> = d.keys().cloned().collect();
        assert_eq!(keys, ["zeta", "size", "font", "custom", "color"]);
        assert_eq!(d["size"], json!(10));
        assert_eq!(schema().sections[1].title, "Colors");
    }

    #[test]
    fn normalize_fills_drops_coerces_clamps() {
        let n = schema()
            .normalize(&map(
                json!({"size": "50", "zeta": "false", "unknown": 1, "font": "b"}),
            ))
            .unwrap();
        assert_eq!(n["size"], json!(20));
        assert_eq!(n["zeta"], json!(false));
        assert_eq!(n["font"], json!("b"));
        assert_eq!(n["color"], json!("#ffffff"));
        assert!(!n.contains_key("unknown"));
        assert_eq!(
            schema().normalize(&map(json!({"size": 1}))).unwrap()["size"],
            json!(5)
        );
    }

    #[test]
    fn normalize_rejects() {
        let errs = schema()
            .normalize(&map(json!({"font": "c", "color": "red", "size": "abc"})))
            .unwrap_err();
        let keys: Vec<_> = errs.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["size", "font", "color"]);
        assert_eq!(errs[2].1, "Use #RRGGBB");
    }

    #[test]
    fn visibility_and_disabled() {
        let s = schema();
        assert!(s.is_visible("zeta", &Map::new()));
        assert!(!s.is_visible("color", &Map::new()));
        assert!(s.is_visible("color", &map(json!({"custom": true}))));
        assert!(!s.is_disabled("color", &Map::new()));
        assert!(s.is_disabled("color", &map(json!({"zeta": false}))));
    }

    #[test]
    fn parse_errors() {
        assert!(SettingsSchema::parse("{}").is_err());
        assert!(
            SettingsSchema::parse(r#"[{"title":"t","settings":{"a":{"type":"color"}}}]"#).is_err()
        );
        assert!(
            SettingsSchema::parse(
                r#"[{"title":"t","settings":{"a":{"type":"string","pattern":"("}}}]"#
            )
            .is_err()
        );
        assert!(SettingsSchema::parse(
            r#"[{"title":"t","settings":{"a":{"type":"string"}}},{"title":"u","settings":{"a":{"type":"string"}}}]"#
        )
        .is_err());
    }
}
