//! Per-widget rendering state: scene, fonts, images and the last layout.

use windows::Win32::Graphics::Direct2D::ID2D1DeviceContext;
use windows::core::Result;

use crate::gpu::Gpu;
use crate::image::{Assets, ImageCache};
use crate::layout::{Laid, Measurer, TextRun, layout};
use crate::paint::Painter;
use crate::scene::{NodeId, Op, Scene};
use crate::style::Transform2D;
use crate::text::TextEngine;

struct Measure<'a> {
    text: &'a mut TextEngine,
    images: &'a mut ImageCache,
    assets: &'a dyn Assets,
}

impl Measurer for Measure<'_> {
    fn measure_text(&mut self, run: &TextRun, max_width: Option<f32>) -> (f32, f32) {
        self.text.measure(run, max_width)
    }

    fn min_text_width(&mut self, run: &TextRun) -> f32 {
        self.text.min_width(run)
    }

    fn image_size(&mut self, src: &str) -> Option<(f32, f32)> {
        self.images.size(src, self.assets)
    }
}

pub struct WidgetRenderer {
    pub scene: Scene,
    text: TextEngine,
    images: ImageCache,
    assets: Box<dyn Assets>,
    laid: Option<Laid>,
    size: (f32, f32),
}

impl WidgetRenderer {
    pub fn new(gpu: &Gpu, assets: Box<dyn Assets>, fonts: &[Vec<u8>]) -> Self {
        let mut text = TextEngine::new(gpu.dwrite.clone());
        if let Err(e) = text.load_fonts(fonts) {
            log::warn!("failed to load widget fonts: {e}");
        }
        Self {
            scene: Scene::new(),
            text,
            images: ImageCache::new(gpu.wic.clone()),
            assets,
            laid: None,
            size: (0.0, 0.0),
        }
    }

    pub fn apply(&mut self, ops: Vec<Op>, now: f64) {
        self.scene.apply_all(ops, now);
    }

    /// Sets the widget size in DIPs.
    pub fn set_size(&mut self, w: f32, h: f32) {
        if self.size != (w, h) {
            self.size = (w, h);
            self.scene.layout_dirty = true;
        }
    }

    pub fn size(&self) -> (f32, f32) {
        self.size
    }

    /// Whether a new frame must be painted.
    pub fn needs_paint(&self, now: f64) -> bool {
        self.scene.layout_dirty
            || self.scene.paint_dirty
            || self.laid.is_none()
            || self.scene.animating(now)
    }

    fn update_layout(&mut self, now: f64) {
        let animating = self.scene.animating(now);
        if self.scene.layout_dirty || self.laid.is_none() || animating {
            let mut m = Measure {
                text: &mut self.text,
                images: &mut self.images,
                assets: self.assets.as_ref(),
            };
            self.laid = Some(layout(&self.scene, self.size.0, self.size.1, &mut m, now));
            self.scene.layout_dirty = false;
        }
        if !animating {
            self.scene.anims.gc(now);
        }
    }

    /// Lays out (if needed) and paints the widget with `base` mapping DIPs to pixels.
    pub fn paint(
        &mut self,
        gpu: &Gpu,
        dc: &ID2D1DeviceContext,
        base: Transform2D,
        now: f64,
    ) -> Result<()> {
        self.update_layout(now);
        self.scene.paint_dirty = false;
        let Some(laid) = &self.laid else {
            return Ok(());
        };
        let mut painter = Painter {
            dc,
            factory: &gpu.d2d_factory,
            text: &mut self.text,
            images: &mut self.images,
            assets: self.assets.as_ref(),
        };
        painter.paint(laid, base)
    }

    /// Deepest interactive node at a point in widget DIPs.
    pub fn hit_test(&self, x: f32, y: f32) -> Option<NodeId> {
        self.laid.as_ref()?.hit_test(x, y)
    }

    pub fn layout_tree(&self) -> Option<&Laid> {
        self.laid.as_ref()
    }
}
