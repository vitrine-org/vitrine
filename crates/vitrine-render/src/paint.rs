//! Painting a laid-out tree with Direct2D.

use std::mem::ManuallyDrop;

use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_RANGE;
use windows::core::{Interface, Result};
use windows_numerics::{Matrix3x2, Vector2};

use crate::color::Color;
use crate::image::{Assets, ImageCache};
use crate::layout::{Content, Fit, Geom, Laid, LineCap, LineJoin, RectF, Shape, Svg, TextRun};
use crate::path::{PathCmd, parse_path};
use crate::style::{BoxPaint, Fill, Transform2D};
use crate::text::TextEngine;

fn d2d_color(c: Color) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: c.r,
        g: c.g,
        b: c.b,
        a: c.a,
    }
}

fn pt(x: f32, y: f32) -> Vector2 {
    Vector2 { X: x, Y: y }
}

fn rect(r: RectF) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.x,
        top: r.y,
        right: r.x + r.w,
        bottom: r.y + r.h,
    }
}

fn matrix(t: Transform2D) -> Matrix3x2 {
    let [a, b, c, d, e, f] = t.0;
    Matrix3x2 {
        M11: a,
        M12: b,
        M21: c,
        M22: d,
        M31: e,
        M32: f,
    }
}

/// Everything needed to paint one widget.
pub struct Painter<'a> {
    pub dc: &'a ID2D1DeviceContext,
    pub factory: &'a ID2D1Factory1,
    pub text: &'a mut TextEngine,
    pub images: &'a mut ImageCache,
    pub assets: &'a dyn Assets,
}

impl Painter<'_> {
    /// Paints the tree. `base` maps DIPs to target pixels (DPI scale + offset).
    pub fn paint(&mut self, root: &Laid, base: Transform2D) -> Result<()> {
        self.node(root, base)
    }

    fn solid(&self, c: Color) -> Result<ID2D1Brush> {
        unsafe { self.dc.CreateSolidColorBrush(&d2d_color(c), None)?.cast() }
    }

    fn fill_brush(&self, fill: &Fill, r: RectF, opacity: f32) -> Result<ID2D1Brush> {
        let stops_of = |stops: &[(f32, Color)]| -> Vec<D2D1_GRADIENT_STOP> {
            stops
                .iter()
                .map(|(p, c)| D2D1_GRADIENT_STOP {
                    position: *p,
                    color: d2d_color(c.with_alpha_mul(opacity)),
                })
                .collect()
        };
        unsafe {
            match fill {
                Fill::Solid(c) => self.solid(c.with_alpha_mul(opacity)),
                Fill::Linear { angle_deg, stops } => {
                    let rt: &ID2D1RenderTarget = self.dc;
                    let collection = rt.CreateGradientStopCollection(
                        &stops_of(stops),
                        D2D1_GAMMA_2_2,
                        D2D1_EXTEND_MODE_CLAMP,
                    )?;
                    // CSS gradient line: through the centre, long enough to touch the corners.
                    let a = angle_deg.to_radians();
                    let (dx, dy) = (a.sin(), -a.cos());
                    let len = (r.w * a.sin()).abs() + (r.h * a.cos()).abs();
                    let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
                    let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                        startPoint: pt(cx - dx * len / 2.0, cy - dy * len / 2.0),
                        endPoint: pt(cx + dx * len / 2.0, cy + dy * len / 2.0),
                    };
                    Ok(self
                        .dc
                        .CreateLinearGradientBrush(&props, None, &collection)?
                        .cast()?)
                }
                Fill::Radial { stops } => {
                    let rt: &ID2D1RenderTarget = self.dc;
                    let collection = rt.CreateGradientStopCollection(
                        &stops_of(stops),
                        D2D1_GAMMA_2_2,
                        D2D1_EXTEND_MODE_CLAMP,
                    )?;
                    let props = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES {
                        center: pt(r.x + r.w / 2.0, r.y + r.h / 2.0),
                        gradientOriginOffset: pt(0.0, 0.0),
                        radiusX: r.w / 2.0,
                        radiusY: r.h / 2.0,
                    };
                    Ok(self
                        .dc
                        .CreateRadialGradientBrush(&props, None, &collection)?
                        .cast()?)
                }
            }
        }
    }

    /// Geometry of a box with per-corner radii.
    fn box_geometry(&self, r: RectF, radius: [f32; 4]) -> Result<ID2D1Geometry> {
        let max = r.w.min(r.h) / 2.0;
        let [tl, tr, br, bl] = radius.map(|v| v.clamp(0.0, max));
        unsafe {
            if tl == tr && tr == br && br == bl {
                let rr = D2D1_ROUNDED_RECT {
                    rect: rect(r),
                    radiusX: tl,
                    radiusY: tl,
                };
                return self.factory.CreateRoundedRectangleGeometry(&rr)?.cast();
            }
            let geo = self.factory.CreatePathGeometry()?;
            let sink = geo.Open()?;
            let (x0, y0, x1, y1) = (r.x, r.y, r.x + r.w, r.y + r.h);
            let arc = |x: f32, y: f32, rad: f32| D2D1_ARC_SEGMENT {
                point: pt(x, y),
                size: D2D_SIZE_F {
                    width: rad,
                    height: rad,
                },
                rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: D2D1_ARC_SIZE_SMALL,
            };
            sink.BeginFigure(pt(x0 + tl, y0), D2D1_FIGURE_BEGIN_FILLED);
            sink.AddLine(pt(x1 - tr, y0));
            if tr > 0.0 {
                sink.AddArc(&arc(x1, y0 + tr, tr));
            }
            sink.AddLine(pt(x1, y1 - br));
            if br > 0.0 {
                sink.AddArc(&arc(x1 - br, y1, br));
            }
            sink.AddLine(pt(x0 + bl, y1));
            if bl > 0.0 {
                sink.AddArc(&arc(x0, y1 - bl, bl));
            }
            sink.AddLine(pt(x0, y0 + tl));
            if tl > 0.0 {
                sink.AddArc(&arc(x0 + tl, y0, tl));
            }
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            sink.Close()?;
            geo.cast()
        }
    }

    fn push_layer(&self, mask: Option<ID2D1Geometry>, opacity: f32) {
        let params = D2D1_LAYER_PARAMETERS1 {
            contentBounds: D2D_RECT_F {
                left: -1e7,
                top: -1e7,
                right: 1e7,
                bottom: 1e7,
            },
            geometricMask: ManuallyDrop::new(mask),
            maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            maskTransform: Matrix3x2::identity(),
            opacity,
            opacityBrush: ManuallyDrop::new(None),
            layerOptions: D2D1_LAYER_OPTIONS1_NONE,
        };
        unsafe { self.dc.PushLayer(&params, None) };
        // The layer keeps its own references; release ours.
        drop(ManuallyDrop::into_inner(params.geometricMask));
    }

    fn node(&mut self, n: &Laid, parent: Transform2D) -> Result<()> {
        let p: &BoxPaint = &n.paint;
        if p.opacity <= 0.0 {
            return Ok(());
        }
        let mut t = parent;
        if let Some(local) = p.transform {
            let (cx, cy) = (n.rect.x + n.rect.w / 2.0, n.rect.y + n.rect.h / 2.0);
            t = Transform2D::translate(-cx, -cy)
                .then(local)
                .then(Transform2D::translate(cx, cy))
                .then(parent);
        }
        unsafe { self.dc.SetTransform(&matrix(t)) };

        let layered = p.opacity < 1.0;
        if layered {
            self.push_layer(None, p.opacity);
        }

        if let Some(shadow) = &p.shadow {
            self.shadow(n.rect, p.radius, shadow)?;
        }
        let needs_geo =
            p.background.is_some() || (p.border_width > 0.0 && p.border_color.is_some()) || p.clip;
        let geo = if needs_geo && n.rect.w > 0.0 && n.rect.h > 0.0 {
            Some(self.box_geometry(n.rect, p.radius)?)
        } else {
            None
        };
        if let (Some(bg), Some(g)) = (&p.background, &geo) {
            let brush = self.fill_brush(bg, n.rect, 1.0)?;
            unsafe { self.dc.FillGeometry(g, &brush, None) };
        }

        let clipped = p.clip && geo.is_some();
        if clipped {
            self.push_layer(geo.clone(), 1.0);
        }

        match &n.content {
            Content::None => {}
            Content::Text(run) => self.text(run, n.content_rect)?,
            Content::Image { src, fit } => self.image(src, *fit, n.content_rect)?,
            Content::Svg(svg) => self.svg(svg, n.content_rect, t)?,
        }
        for c in &n.children {
            self.node(c, t)?;
            unsafe { self.dc.SetTransform(&matrix(t)) };
        }

        if clipped {
            unsafe { self.dc.PopLayer() };
        }
        if let (Some(color), true) = (p.border_color, p.border_width > 0.0) {
            let w = p.border_width;
            let inner = RectF {
                x: n.rect.x + w / 2.0,
                y: n.rect.y + w / 2.0,
                w: n.rect.w - w,
                h: n.rect.h - w,
            };
            if inner.w > 0.0 && inner.h > 0.0 {
                let g = self.box_geometry(inner, p.radius.map(|r| (r - w / 2.0).max(0.0)))?;
                let brush = self.solid(color)?;
                unsafe { self.dc.DrawGeometry(&g, &brush, w, None) };
            }
        }
        if layered {
            unsafe { self.dc.PopLayer() };
        }
        Ok(())
    }

    /// Soft shadow approximated by stacked translucent rounded rects.
    fn shadow(&self, r: RectF, radius: [f32; 4], s: &crate::style::Shadow) -> Result<()> {
        let steps = (s.blur / 2.0).ceil().clamp(1.0, 8.0) as i32;
        let per = s.color.a / steps as f32;
        for i in 0..steps {
            let grow = s.blur * (i as f32 + 1.0) / steps as f32 / 2.0;
            let rr = RectF {
                x: r.x + s.dx - grow,
                y: r.y + s.dy - grow,
                w: r.w + grow * 2.0,
                h: r.h + grow * 2.0,
            };
            let g = self.box_geometry(rr, radius.map(|v| v + grow))?;
            let brush = self.solid(Color {
                a: per * 0.6,
                ..s.color
            })?;
            unsafe { self.dc.FillGeometry(&g, &brush, None) };
        }
        Ok(())
    }

    fn text(&mut self, run: &TextRun, r: RectF) -> Result<()> {
        let layout = self
            .text
            .layout(run, if run.base.wrap { Some(r.w) } else { None })?;
        unsafe {
            if !run.base.wrap {
                // Alignment needs the box width even without wrapping.
                layout.SetMaxWidth(r.w.max(1.0))?;
            }
            let mut pos = 0u32;
            for seg in &run.segments {
                let len = seg.text.encode_utf16().count() as u32;
                if len > 0 {
                    let brush = self.solid(seg.style.color)?;
                    layout.SetDrawingEffect(
                        &brush,
                        DWRITE_TEXT_RANGE {
                            startPosition: pos,
                            length: len,
                        },
                    )?;
                }
                pos += len;
            }
            let default = self.solid(run.base.color)?;
            self.dc.DrawTextLayout(
                pt(r.x, r.y),
                &layout,
                &default,
                D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
            );
        }
        Ok(())
    }

    fn image(&mut self, src: &str, fit: Fit, r: RectF) -> Result<()> {
        let Some(wic) = self.images.get(src, self.assets) else {
            return Ok(());
        };
        unsafe {
            let bmp = self.dc.CreateBitmapFromWicBitmap(&wic, None)?;
            let size = bmp.GetSize();
            if size.width <= 0.0 || size.height <= 0.0 {
                return Ok(());
            }
            let (sx, sy) = (r.w / size.width, r.h / size.height);
            let dest = match fit {
                Fit::Fill => r,
                Fit::Contain | Fit::Cover => {
                    let s = if fit == Fit::Contain {
                        sx.min(sy)
                    } else {
                        sx.max(sy)
                    };
                    let (w, h) = (size.width * s, size.height * s);
                    RectF {
                        x: r.x + (r.w - w) / 2.0,
                        y: r.y + (r.h - h) / 2.0,
                        w,
                        h,
                    }
                }
            };
            if fit == Fit::Cover {
                self.dc
                    .PushAxisAlignedClip(&rect(r), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            }
            self.dc.DrawBitmap(
                &bmp,
                Some(&rect(dest)),
                1.0,
                D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
                None,
                None,
            );
            if fit == Fit::Cover {
                self.dc.PopAxisAlignedClip();
            }
        }
        Ok(())
    }

    fn svg(&mut self, svg: &Svg, r: RectF, parent: Transform2D) -> Result<()> {
        let vb = svg.view_box.unwrap_or([0.0, 0.0, r.w, r.h]);
        let (vw, vh) = (vb[2].max(1e-6), vb[3].max(1e-6));
        let s = (r.w / vw).min(r.h / vh);
        let (ox, oy) = (r.x + (r.w - vw * s) / 2.0, r.y + (r.h - vh * s) / 2.0);
        let view = Transform2D::translate(-vb[0], -vb[1])
            .then(Transform2D::scale(s, s))
            .then(Transform2D::translate(ox, oy));
        for shape in &svg.shapes {
            self.shape(shape, view.then(parent), 1.0)?;
        }
        unsafe { self.dc.SetTransform(&matrix(parent)) };
        Ok(())
    }

    fn shape_geometry(&self, geom: &Geom) -> Result<Option<ID2D1Geometry>> {
        unsafe {
            Ok(Some(match geom {
                Geom::Group => return Ok(None),
                Geom::Circle { cx, cy, r } => self
                    .factory
                    .CreateEllipseGeometry(&D2D1_ELLIPSE {
                        point: pt(*cx, *cy),
                        radiusX: *r,
                        radiusY: *r,
                    })?
                    .cast()?,
                Geom::Ellipse { cx, cy, rx, ry } => self
                    .factory
                    .CreateEllipseGeometry(&D2D1_ELLIPSE {
                        point: pt(*cx, *cy),
                        radiusX: *rx,
                        radiusY: *ry,
                    })?
                    .cast()?,
                Geom::Rect { x, y, w, h, rx, ry } => self
                    .factory
                    .CreateRoundedRectangleGeometry(&D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: *x,
                            top: *y,
                            right: x + w,
                            bottom: y + h,
                        },
                        radiusX: *rx,
                        radiusY: *ry,
                    })?
                    .cast()?,
                Geom::Line { x1, y1, x2, y2 } => {
                    let geo = self.factory.CreatePathGeometry()?;
                    let sink = geo.Open()?;
                    sink.BeginFigure(pt(*x1, *y1), D2D1_FIGURE_BEGIN_HOLLOW);
                    sink.AddLine(pt(*x2, *y2));
                    sink.EndFigure(D2D1_FIGURE_END_OPEN);
                    sink.Close()?;
                    geo.cast()?
                }
                Geom::Path(d) => {
                    let geo = self.factory.CreatePathGeometry()?;
                    let sink = geo.Open()?;
                    let mut open = false;
                    for cmd in parse_path(d) {
                        match cmd {
                            PathCmd::Move(x, y) => {
                                if open {
                                    sink.EndFigure(D2D1_FIGURE_END_OPEN);
                                }
                                sink.BeginFigure(pt(x, y), D2D1_FIGURE_BEGIN_FILLED);
                                open = true;
                            }
                            PathCmd::Line(x, y) if open => sink.AddLine(pt(x, y)),
                            PathCmd::Cubic(x1, y1, x2, y2, x, y) if open => {
                                sink.AddBezier(&D2D1_BEZIER_SEGMENT {
                                    point1: pt(x1, y1),
                                    point2: pt(x2, y2),
                                    point3: pt(x, y),
                                })
                            }
                            PathCmd::Quad(x1, y1, x, y) if open => {
                                sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT {
                                    point1: pt(x1, y1),
                                    point2: pt(x, y),
                                })
                            }
                            PathCmd::Arc(rx, ry, rot, large, sweep, x, y) if open => {
                                sink.AddArc(&D2D1_ARC_SEGMENT {
                                    point: pt(x, y),
                                    size: D2D_SIZE_F {
                                        width: rx,
                                        height: ry,
                                    },
                                    rotationAngle: rot,
                                    sweepDirection: if sweep {
                                        D2D1_SWEEP_DIRECTION_CLOCKWISE
                                    } else {
                                        D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE
                                    },
                                    arcSize: if large {
                                        D2D1_ARC_SIZE_LARGE
                                    } else {
                                        D2D1_ARC_SIZE_SMALL
                                    },
                                })
                            }
                            PathCmd::Close if open => {
                                sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                                open = false;
                            }
                            _ => {}
                        }
                    }
                    if open {
                        sink.EndFigure(D2D1_FIGURE_END_OPEN);
                    }
                    sink.Close()?;
                    geo.cast()?
                }
            }))
        }
    }

    fn stroke_style(&self, s: &Shape) -> Result<Option<ID2D1StrokeStyle>> {
        let cap = match s.cap {
            LineCap::Butt => D2D1_CAP_STYLE_FLAT,
            LineCap::Round => D2D1_CAP_STYLE_ROUND,
            LineCap::Square => D2D1_CAP_STYLE_SQUARE,
        };
        let join = match s.join {
            LineJoin::Miter => D2D1_LINE_JOIN_MITER,
            LineJoin::Round => D2D1_LINE_JOIN_ROUND,
            LineJoin::Bevel => D2D1_LINE_JOIN_BEVEL,
        };
        let w = s.stroke_width.max(1e-3);
        // SVG dash arrays are in user units; D2D wants multiples of the stroke width.
        let mut dashes: Vec<f32> = s.dash.iter().map(|d| d.max(0.0) / w).collect();
        if dashes.len() % 2 == 1 {
            dashes = dashes.repeat(2);
        }
        let dashed = !dashes.is_empty() && dashes.iter().any(|d| *d > 0.0);
        if !dashed && cap == D2D1_CAP_STYLE_FLAT && join == D2D1_LINE_JOIN_MITER {
            return Ok(None);
        }
        let props = D2D1_STROKE_STYLE_PROPERTIES {
            startCap: cap,
            endCap: cap,
            dashCap: cap,
            lineJoin: join,
            miterLimit: 4.0,
            dashStyle: if dashed {
                D2D1_DASH_STYLE_CUSTOM
            } else {
                D2D1_DASH_STYLE_SOLID
            },
            dashOffset: s.dash_offset / w,
        };
        unsafe {
            let base: &ID2D1Factory = self.factory;
            let st = base.CreateStrokeStyle(&props, if dashed { Some(&dashes) } else { None })?;
            Ok(Some(st))
        }
    }

    fn shape(&mut self, s: &Shape, parent: Transform2D, opacity: f32) -> Result<()> {
        let t = s.transform.then(parent);
        let opacity = opacity * s.opacity;
        if opacity <= 0.0 {
            return Ok(());
        }
        if matches!(s.geom, Geom::Group) {
            for c in &s.children {
                self.shape(c, t, opacity)?;
            }
            return Ok(());
        }
        let Some(geo) = self.shape_geometry(&s.geom)? else {
            return Ok(());
        };
        unsafe {
            self.dc.SetTransform(&matrix(t));
            if let Some(fill) = s.fill {
                let brush = self.solid(fill.with_alpha_mul(opacity))?;
                self.dc.FillGeometry(&geo, &brush, None);
            }
            if let Some(stroke) = s.stroke {
                // A zero-length dash pattern hides the stroke (e.g. an empty gauge).
                if !s.dash.is_empty()
                    && s.dash.iter().step_by(2).all(|d| *d <= 0.0)
                    && s.cap == LineCap::Butt
                {
                    return Ok(());
                }
                let style = self.stroke_style(s)?;
                let brush = self.solid(stroke.with_alpha_mul(opacity))?;
                self.dc
                    .DrawGeometry(&geo, &brush, s.stroke_width, style.as_ref());
            }
        }
        Ok(())
    }
}
