//! Image decoding with WIC. Sources are package-relative paths or `data:` URIs.

use std::collections::HashMap;
use std::path::PathBuf;

use base64::Engine;
use windows::Win32::Graphics::Imaging::*;
use windows::core::Result;

/// Resolves asset paths used by a widget.
pub trait Assets {
    fn read(&self, path: &str) -> Option<Vec<u8>>;
}

/// Assets read from a directory, plus `data:` URIs.
pub struct DirAssets(pub PathBuf);

impl Assets for DirAssets {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        if let Some(data) = decode_data_uri(path) {
            return Some(data);
        }
        let rel = path.trim_start_matches("./").trim_start_matches('/');
        if rel.split(['/', '\\']).any(|p| p == "..") {
            return None;
        }
        std::fs::read(self.0.join(rel)).ok()
    }
}

pub fn decode_data_uri(s: &str) -> Option<Vec<u8>> {
    let rest = s.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    if meta.ends_with(";base64") {
        base64::engine::general_purpose::STANDARD
            .decode(data.trim())
            .ok()
    } else {
        Some(percent_decode(data))
    }
}

fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// Decoded images, converted to premultiplied BGRA and owned in memory.
pub struct ImageCache {
    wic: IWICImagingFactory,
    images: HashMap<String, Option<IWICBitmap>>,
}

impl ImageCache {
    pub fn new(wic: IWICImagingFactory) -> Self {
        Self {
            wic,
            images: HashMap::new(),
        }
    }

    pub fn get(&mut self, src: &str, assets: &dyn Assets) -> Option<IWICBitmap> {
        if let Some(v) = self.images.get(src) {
            return v.clone();
        }
        let decoded = assets.read(src).and_then(|bytes| self.decode(&bytes).ok());
        self.images.insert(src.to_string(), decoded.clone());
        decoded
    }

    pub fn size(&mut self, src: &str, assets: &dyn Assets) -> Option<(f32, f32)> {
        let bmp = self.get(src, assets)?;
        let (mut w, mut h) = (0u32, 0u32);
        unsafe { bmp.GetSize(&mut w, &mut h).ok()? };
        Some((w as f32, h as f32))
    }

    fn decode(&self, bytes: &[u8]) -> Result<IWICBitmap> {
        unsafe {
            let stream = self.wic.CreateStream()?;
            stream.InitializeFromMemory(bytes)?;
            let decoder = self.wic.CreateDecoderFromStream(
                &stream,
                std::ptr::null(),
                WICDecodeMetadataCacheOnLoad,
            )?;
            let frame = decoder.GetFrame(0)?;
            let converter = self.wic.CreateFormatConverter()?;
            converter.Initialize(
                &frame,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeMedianCut,
            )?;
            // Copy into memory so the stream can be dropped.
            self.wic
                .CreateBitmapFromSource(&converter, WICBitmapCacheOnLoad)
        }
    }
}
