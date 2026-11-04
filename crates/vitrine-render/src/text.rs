//! DirectWrite text layout: measurement for the layout pass and layouts for painting.

use std::collections::{HashMap, HashSet};

use windows::Win32::Graphics::DirectWrite::*;
use windows::core::{HSTRING, Interface, PCWSTR, Result};

use crate::layout::TextRun;
use crate::style::{TextAlign, TextStyle};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn weight(w: u16) -> DWRITE_FONT_WEIGHT {
    DWRITE_FONT_WEIGHT(w.clamp(1, 999) as i32)
}

fn font_style(italic: bool) -> DWRITE_FONT_STYLE {
    if italic {
        DWRITE_FONT_STYLE_ITALIC
    } else {
        DWRITE_FONT_STYLE_NORMAL
    }
}

/// Text engine for one widget: system fonts plus the fonts shipped in its package.
pub struct TextEngine {
    factory: IDWriteFactory5,
    custom: Option<IDWriteFontCollection1>,
    custom_families: HashSet<String>,
    formats: HashMap<(String, u16, bool, u32), IDWriteTextFormat>,
    layouts: HashMap<(String, u32), IDWriteTextLayout>,
}

impl TextEngine {
    pub fn new(factory: IDWriteFactory5) -> Self {
        Self {
            factory,
            custom: None,
            custom_families: HashSet::new(),
            formats: HashMap::new(),
            layouts: HashMap::new(),
        }
    }

    /// Registers font files (TTF/OTF bytes). Returns the family names found.
    pub fn load_fonts(&mut self, files: &[Vec<u8>]) -> Result<Vec<String>> {
        if files.is_empty() {
            return Ok(Vec::new());
        }
        unsafe {
            let loader = self.factory.CreateInMemoryFontFileLoader()?;
            self.factory.RegisterFontFileLoader(&loader)?;
            let builder = self.factory.CreateFontSetBuilder()?;
            for data in files {
                let file = loader.CreateInMemoryFontFileReference(
                    &self.factory,
                    data.as_ptr().cast(),
                    data.len() as u32,
                    None,
                )?;
                builder.AddFontFile(&file)?;
            }
            let set = builder.CreateFontSet()?;
            let collection = self.factory.CreateFontCollectionFromFontSet(&set)?;
            let mut names = Vec::new();
            for i in 0..collection.GetFontFamilyCount() {
                let family = collection.GetFontFamily(i)?;
                let strings = family.GetFamilyNames()?;
                for j in 0..strings.GetCount() {
                    let len = strings.GetStringLength(j)? as usize;
                    let mut buf = vec![0u16; len + 1];
                    strings.GetString(j, &mut buf)?;
                    let name = String::from_utf16_lossy(&buf[..len]);
                    self.custom_families.insert(name.to_lowercase());
                    names.push(name);
                }
            }
            self.custom = Some(collection);
            self.formats.clear();
            self.layouts.clear();
            Ok(names)
        }
    }

    fn collection_for(&self, family: &str) -> Option<IDWriteFontCollection> {
        if self.custom_families.contains(&family.to_lowercase()) {
            self.custom.as_ref().and_then(|c| c.cast().ok())
        } else {
            None
        }
    }

    fn format(&mut self, st: &TextStyle) -> Result<IDWriteTextFormat> {
        let key = (st.family.clone(), st.weight, st.italic, st.size.to_bits());
        if let Some(f) = self.formats.get(&key) {
            return Ok(f.clone());
        }
        let family = HSTRING::from(st.family.as_str());
        let collection = self.collection_for(&st.family);
        let f = unsafe {
            self.factory.CreateTextFormat(
                &family,
                collection.as_ref(),
                weight(st.weight),
                font_style(st.italic),
                DWRITE_FONT_STRETCH_NORMAL,
                st.size.max(1.0),
                &HSTRING::from("en-us"),
            )?
        };
        self.formats.insert(key, f.clone());
        Ok(f)
    }

    /// A layout for `run` with the given maximum width (`None` = unbounded).
    pub fn layout(&mut self, run: &TextRun, max_width: Option<f32>) -> Result<IDWriteTextLayout> {
        let width = max_width.unwrap_or(f32::MAX / 4.0).max(0.0);
        let key = (format!("{run:?}"), width.to_bits());
        if let Some(l) = self.layouts.get(&key) {
            return Ok(l.clone());
        }
        if self.layouts.len() > 256 {
            self.layouts.clear();
        }
        let base = &run.base;
        let format = self.format(base)?;
        let text = wide(&run.text());
        unsafe {
            let layout = self
                .factory
                .CreateTextLayout(&text, &format, width, f32::MAX / 4.0)?;
            layout.SetWordWrapping(if base.wrap {
                DWRITE_WORD_WRAPPING_WRAP
            } else {
                DWRITE_WORD_WRAPPING_NO_WRAP
            })?;
            layout.SetTextAlignment(match base.align {
                TextAlign::Start => DWRITE_TEXT_ALIGNMENT_LEADING,
                TextAlign::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                TextAlign::End => DWRITE_TEXT_ALIGNMENT_TRAILING,
                TextAlign::Justify => DWRITE_TEXT_ALIGNMENT_JUSTIFIED,
            })?;
            if let Some(lh) = base.line_height {
                layout.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, lh, lh * 0.8)?;
            }
            if base.ellipsis {
                let sign = self.factory.CreateEllipsisTrimmingSign(&format)?;
                let trimming = DWRITE_TRIMMING {
                    granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                    delimiter: 0,
                    delimiterCount: 0,
                };
                layout.SetTrimming(&trimming, &sign)?;
            }
            let layout1: Option<IDWriteTextLayout1> = layout.cast().ok();
            let mut pos = 0u32;
            for seg in &run.segments {
                let len = seg.text.encode_utf16().count() as u32;
                if len == 0 {
                    continue;
                }
                let range = DWRITE_TEXT_RANGE {
                    startPosition: pos,
                    length: len,
                };
                let st = &seg.style;
                if st.weight != base.weight {
                    layout.SetFontWeight(weight(st.weight), range)?;
                }
                if st.italic != base.italic {
                    layout.SetFontStyle(font_style(st.italic), range)?;
                }
                if st.size != base.size {
                    layout.SetFontSize(st.size, range)?;
                }
                if st.family != base.family {
                    let fam = HSTRING::from(st.family.as_str());
                    layout.SetFontFamilyName(PCWSTR(fam.as_ptr()), range)?;
                    layout.SetFontCollection(self.collection_for(&st.family).as_ref(), range)?;
                }
                if st.underline {
                    layout.SetUnderline(true, range)?;
                }
                if st.strike {
                    layout.SetStrikethrough(true, range)?;
                }
                if st.letter_spacing != 0.0
                    && let Some(l1) = &layout1
                {
                    l1.SetCharacterSpacing(0.0, st.letter_spacing, 0.0, range)?;
                }
                pos += len;
            }
            self.layouts.insert(key, layout.clone());
            Ok(layout)
        }
    }

    pub fn measure(&mut self, run: &TextRun, max_width: Option<f32>) -> (f32, f32) {
        let Ok(layout) = self.layout(run, max_width) else {
            return (0.0, 0.0);
        };
        let mut m = DWRITE_TEXT_METRICS::default();
        if unsafe { layout.GetMetrics(&mut m) }.is_err() {
            return (0.0, 0.0);
        }
        (m.widthIncludingTrailingWhitespace.ceil(), m.height.ceil())
    }

    pub fn min_width(&mut self, run: &TextRun) -> f32 {
        self.layout(run, None)
            .and_then(|l| unsafe { l.DetermineMinWidth() })
            .map(|w| w.ceil())
            .unwrap_or(0.0)
    }
}
