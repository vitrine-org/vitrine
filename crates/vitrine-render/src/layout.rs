//! Layout pass: turns a [`Scene`] into a tree of positioned boxes ([`Laid`])
//! ready to be painted. Pure Rust; text and image measurement are delegated to
//! a [`Measurer`] so the pass can be tested without DirectWrite.

use taffy::compute_leaf_layout;
use taffy::prelude::*;
use taffy::style::CompactLength;

use crate::color::Color;
use crate::scene::{Kind, NodeId, ROOT, Scene};
use crate::style::{
    BoxPaint, StyleSource, TextStyle, Transform2D, box_paint, layout_style, parse_color,
    parse_transform,
};
use crate::value::PropValue;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl RectF {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub text: String,
    pub style: TextStyle,
}

/// A paragraph: styled segments sharing paragraph-level properties of `base`.
#[derive(Clone, Debug, PartialEq)]
pub struct TextRun {
    pub segments: Vec<Segment>,
    pub base: TextStyle,
}

impl TextRun {
    pub fn text(&self) -> String {
        self.segments.iter().map(|s| s.text.as_str()).collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    Contain,
    Cover,
    Fill,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineCap {
    Butt,
    Round,
    Square,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineJoin {
    Miter,
    Round,
    Bevel,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Geom {
    Group,
    Circle {
        cx: f32,
        cy: f32,
        r: f32,
    },
    Ellipse {
        cx: f32,
        cy: f32,
        rx: f32,
        ry: f32,
    },
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        rx: f32,
        ry: f32,
    },
    Line {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
    },
    Path(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    pub geom: Geom,
    pub fill: Option<Color>,
    pub stroke: Option<Color>,
    pub stroke_width: f32,
    pub dash: Vec<f32>,
    pub dash_offset: f32,
    pub cap: LineCap,
    pub join: LineJoin,
    pub opacity: f32,
    pub transform: Transform2D,
    pub children: Vec<Shape>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Svg {
    pub view_box: Option<[f32; 4]>,
    pub shapes: Vec<Shape>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    None,
    Text(TextRun),
    Image { src: String, fit: Fit },
    Svg(Svg),
}

/// A positioned box. Rects are absolute, in DIPs, relative to the widget origin.
#[derive(Clone, Debug, PartialEq)]
pub struct Laid {
    pub id: NodeId,
    /// Border box.
    pub rect: RectF,
    /// Content box (inside padding and border).
    pub content_rect: RectF,
    pub content: Content,
    pub paint: BoxPaint,
    pub interactive: bool,
    pub children: Vec<Laid>,
}

impl Laid {
    /// Deepest interactive node containing the point, if any.
    pub fn hit_test(&self, x: f32, y: f32) -> Option<NodeId> {
        if !self.rect.contains(x, y) && self.paint.clip {
            return None;
        }
        for c in self.children.iter().rev() {
            if let Some(hit) = c.hit_test(x, y) {
                return Some(hit);
            }
        }
        (self.interactive && self.rect.contains(x, y)).then_some(self.id)
    }

    /// Deepest node of any kind containing the point (used for event targeting).
    pub fn node_at(&self, x: f32, y: f32) -> Option<NodeId> {
        if !self.rect.contains(x, y) {
            return None;
        }
        for c in self.children.iter().rev() {
            if let Some(hit) = c.node_at(x, y) {
                return Some(hit);
            }
        }
        Some(self.id)
    }
}

pub trait Measurer {
    /// Size of the laid out paragraph; `max_width: None` means no wrapping limit.
    fn measure_text(&mut self, run: &TextRun, max_width: Option<f32>) -> (f32, f32);
    /// Width of the longest unbreakable fragment.
    fn min_text_width(&mut self, run: &TextRun) -> f32;
    /// Intrinsic size of an image in DIPs.
    fn image_size(&mut self, src: &str) -> Option<(f32, f32)>;
}

struct NodeSrc<'a> {
    scene: &'a Scene,
    id: NodeId,
    now: f64,
}

impl StyleSource for NodeSrc<'_> {
    fn style(&self, key: &str) -> Option<PropValue> {
        self.scene.style_value(self.id, key, self.now)
    }
}

enum Leaf {
    Text(usize),
    Image(String),
}

struct Plan {
    id: NodeId,
    node: taffy::NodeId,
    content: Content,
    paint: BoxPaint,
    interactive: bool,
    children: Vec<usize>,
}

struct Builder<'a> {
    scene: &'a Scene,
    now: f64,
    tree: TaffyTree<Leaf>,
    runs: Vec<TextRun>,
    plans: Vec<Plan>,
}

impl Builder<'_> {
    fn src(&self, id: NodeId) -> NodeSrc<'_> {
        NodeSrc {
            scene: self.scene,
            id,
            now: self.now,
        }
    }

    fn attr(&self, id: NodeId, key: &str) -> Option<PropValue> {
        self.scene
            .attr_value(id, key, self.now)
            .or_else(|| self.scene.style_value(id, key, self.now))
    }

    fn attr_f(&self, id: NodeId, key: &str, default: f32) -> f32 {
        self.attr(id, key)
            .and_then(|v| v.as_f32())
            .unwrap_or(default)
    }

    fn build(
        &mut self,
        id: NodeId,
        parent_ts: &TextStyle,
        size_override: Option<(f32, f32)>,
    ) -> Option<usize> {
        let node = self.scene.node(id)?;
        let kind = node.kind;
        let interactive = node.interactive;
        if kind == Kind::Br || kind.is_shape() && kind != Kind::Group {
            return None;
        }
        if kind == Kind::TextNode {
            if node.text.is_empty() {
                return None;
            }
            let run = TextRun {
                segments: vec![Segment {
                    text: parent_ts.apply_case(&node.text),
                    style: parent_ts.clone(),
                }],
                base: parent_ts.clone(),
            };
            return Some(self.push_text_leaf(
                id,
                Style::default(),
                run,
                BoxPaint::default(),
                false,
            ));
        }

        let ts = TextStyle::inherit(parent_ts, &self.src(id));
        let mut style = layout_style(&self.src(id));
        let paint = box_paint(&self.src(id), ts.color);
        if let Some((w, h)) = size_override {
            style.size = Size {
                width: length(w),
                height: length(h),
            };
        }

        match kind {
            Kind::Text => {
                let mut segments = Vec::new();
                self.collect_text(id, &ts, &mut segments);
                let run = TextRun { segments, base: ts };
                Some(self.push_text_leaf(id, style, run, paint, interactive))
            }
            Kind::Image => {
                let src = self
                    .attr(id, "src")
                    .map(|v| v.to_string())
                    .unwrap_or_default();
                let fit = match self
                    .attr(id, "objectFit")
                    .or_else(|| self.attr(id, "fit"))
                    .map(|v| v.to_string())
                {
                    Some(f) if f == "cover" => Fit::Cover,
                    Some(f) if f == "fill" => Fit::Fill,
                    _ => Fit::Contain,
                };
                let node = self
                    .tree
                    .new_leaf_with_context(style, Leaf::Image(src.clone()))
                    .ok()?;
                Some(self.push(Plan {
                    id,
                    node,
                    content: Content::Image { src, fit },
                    paint,
                    interactive,
                    children: vec![],
                }))
            }
            Kind::Svg => {
                let view_box = self
                    .attr(id, "viewBox")
                    .map(|v| v.as_numbers())
                    .filter(|v| v.len() == 4);
                let view_box = view_box.map(|v| [v[0], v[1], v[2], v[3]]);
                // width/height attributes act as the default size
                if matches!(style.size.width, d if d == auto()) {
                    if let Some(w) = self.attr(id, "width").and_then(|v| v.as_f32()) {
                        style.size.width = length(w);
                    }
                }
                if matches!(style.size.height, d if d == auto()) {
                    if let Some(h) = self.attr(id, "height").and_then(|v| v.as_f32()) {
                        style.size.height = length(h);
                    }
                }
                // With only one dimension known, derive the other from the viewBox
                // (explicitly, so that `align-items: stretch` does not distort it).
                if let Some(vb) = view_box.filter(|v| v[2] > 0.0 && v[3] > 0.0) {
                    let fixed = |d: Dimension| {
                        (!d.is_auto() && d.tag() == CompactLength::LENGTH_TAG).then(|| d.value())
                    };
                    match (fixed(style.size.width), fixed(style.size.height)) {
                        (Some(w), None) if style.size.height.is_auto() => {
                            style.size.height = length(w * vb[3] / vb[2])
                        }
                        (None, Some(h)) if style.size.width.is_auto() => {
                            style.size.width = length(h * vb[2] / vb[3])
                        }
                        (None, None)
                            if style.size.width.is_auto() && style.size.height.is_auto() =>
                        {
                            style.size = Size {
                                width: length(vb[2]),
                                height: length(vb[3]),
                            };
                        }
                        _ => {}
                    }
                }
                let shapes = self.collect_shapes(id, ts.color);
                let node = self.tree.new_leaf(style).ok()?;
                Some(self.push(Plan {
                    id,
                    node,
                    content: Content::Svg(Svg { view_box, shapes }),
                    paint,
                    interactive,
                    children: vec![],
                }))
            }
            _ => {
                let child_ids = self.scene.node(id)?.children.clone();
                let mut kids = Vec::new();
                for c in child_ids {
                    if let Some(k) = self.build(c, &ts, None) {
                        kids.push(k);
                    }
                }
                let taffy_kids: Vec<taffy::NodeId> =
                    kids.iter().map(|k| self.plans[*k].node).collect();
                let node = self.tree.new_with_children(style, &taffy_kids).ok()?;
                Some(self.push(Plan {
                    id,
                    node,
                    content: Content::None,
                    paint,
                    interactive,
                    children: kids,
                }))
            }
        }
    }

    fn push(&mut self, plan: Plan) -> usize {
        self.plans.push(plan);
        self.plans.len() - 1
    }

    fn push_text_leaf(
        &mut self,
        id: NodeId,
        style: Style,
        run: TextRun,
        paint: BoxPaint,
        interactive: bool,
    ) -> usize {
        self.runs.push(run.clone());
        let node = self
            .tree
            .new_leaf_with_context(style, Leaf::Text(self.runs.len() - 1))
            .unwrap();
        self.push(Plan {
            id,
            node,
            content: Content::Text(run),
            paint,
            interactive,
            children: vec![],
        })
    }

    fn collect_text(&self, id: NodeId, ts: &TextStyle, out: &mut Vec<Segment>) {
        let Some(node) = self.scene.node(id) else {
            return;
        };
        for &c in &node.children {
            let Some(child) = self.scene.node(c) else {
                continue;
            };
            match child.kind {
                Kind::TextNode => out.push(Segment {
                    text: ts.apply_case(&child.text),
                    style: ts.clone(),
                }),
                Kind::Br => out.push(Segment {
                    text: "\n".into(),
                    style: ts.clone(),
                }),
                Kind::Text | Kind::View => {
                    let child_ts = TextStyle::inherit(ts, &self.src(c));
                    self.collect_text(c, &child_ts, out);
                }
                _ => {}
            }
        }
    }

    fn color_attr(&self, id: NodeId, key: &str, current: Color) -> Option<Option<Color>> {
        let v = self.attr(id, key)?;
        let s = v.to_string();
        if s == "none" || s == "transparent" {
            return Some(None);
        }
        Some(parse_color(&s, current))
    }

    fn collect_shapes(&self, id: NodeId, current: Color) -> Vec<Shape> {
        let Some(node) = self.scene.node(id) else {
            return vec![];
        };
        let mut out = Vec::new();
        for &c in &node.children {
            let Some(child) = self.scene.node(c) else {
                continue;
            };
            if !child.kind.is_shape() {
                continue;
            }
            let color = self
                .attr(c, "color")
                .and_then(|v| parse_color(&v.to_string(), current))
                .unwrap_or(current);
            let f = |k: &str, d: f32| self.attr_f(c, k, d);
            let geom = match child.kind {
                Kind::Group => Geom::Group,
                Kind::Circle => Geom::Circle {
                    cx: f("cx", 0.0),
                    cy: f("cy", 0.0),
                    r: f("r", 0.0),
                },
                Kind::Ellipse => Geom::Ellipse {
                    cx: f("cx", 0.0),
                    cy: f("cy", 0.0),
                    rx: f("rx", 0.0),
                    ry: f("ry", 0.0),
                },
                Kind::Rect => {
                    let rx = self.attr(c, "rx").and_then(|v| v.as_f32());
                    let ry = self.attr(c, "ry").and_then(|v| v.as_f32());
                    Geom::Rect {
                        x: f("x", 0.0),
                        y: f("y", 0.0),
                        w: f("width", 0.0),
                        h: f("height", 0.0),
                        rx: rx.or(ry).unwrap_or(0.0),
                        ry: ry.or(rx).unwrap_or(0.0),
                    }
                }
                Kind::Line => Geom::Line {
                    x1: f("x1", 0.0),
                    y1: f("y1", 0.0),
                    x2: f("x2", 0.0),
                    y2: f("y2", 0.0),
                },
                _ => Geom::Path(self.attr(c, "d").map(|v| v.to_string()).unwrap_or_default()),
            };
            let default_fill = if matches!(geom, Geom::Line { .. } | Geom::Group) {
                None
            } else {
                Some(Color::BLACK)
            };
            let fill = self.color_attr(c, "fill", color).unwrap_or(default_fill);
            let fill_opacity = f("fillOpacity", 1.0);
            let stroke = self.color_attr(c, "stroke", color).flatten();
            let stroke_opacity = f("strokeOpacity", 1.0);
            out.push(Shape {
                geom,
                fill: fill.map(|c| c.with_alpha_mul(fill_opacity)),
                stroke: stroke.map(|c| c.with_alpha_mul(stroke_opacity)),
                stroke_width: f("strokeWidth", 1.0),
                dash: self
                    .attr(c, "strokeDasharray")
                    .map(|v| v.as_numbers())
                    .unwrap_or_default(),
                dash_offset: f("strokeDashoffset", 0.0),
                cap: match self
                    .attr(c, "strokeLinecap")
                    .map(|v| v.to_string())
                    .as_deref()
                {
                    Some("round") => LineCap::Round,
                    Some("square") => LineCap::Square,
                    _ => LineCap::Butt,
                },
                join: match self
                    .attr(c, "strokeLinejoin")
                    .map(|v| v.to_string())
                    .as_deref()
                {
                    Some("round") => LineJoin::Round,
                    Some("bevel") => LineJoin::Bevel,
                    _ => LineJoin::Miter,
                },
                opacity: f("opacity", 1.0).clamp(0.0, 1.0),
                transform: self
                    .attr(c, "transform")
                    .map(|v| parse_transform(&v.to_string()))
                    .unwrap_or(Transform2D::IDENTITY),
                children: if child.kind == Kind::Group {
                    self.collect_shapes(c, color)
                } else {
                    vec![]
                },
            });
        }
        out
    }
}

/// Lays out the scene for a widget of `width` × `height` DIPs.
pub fn layout(
    scene: &Scene,
    width: f32,
    height: f32,
    measurer: &mut dyn Measurer,
    now: f64,
) -> Laid {
    let mut b = Builder {
        scene,
        now,
        tree: TaffyTree::new(),
        runs: Vec::new(),
        plans: Vec::new(),
    };
    let root_ts = TextStyle::default();
    let root = b
        .build(ROOT, &root_ts, Some((width, height)))
        .expect("root exists");
    let root_node = b.plans[root].node;
    let runs = std::mem::take(&mut b.runs);
    let _ = b.tree.compute_layout_with_measure(
        root_node,
        Size {
            width: AvailableSpace::Definite(width),
            height: AvailableSpace::Definite(height),
        },
        |inputs, _id, ctx, style| {
            compute_leaf_layout(
                inputs,
                style,
                |_, v| v,
                |known, available| match ctx {
                    Some(Leaf::Text(i)) => {
                        let run = &runs[*i];
                        let max = match (known.width, available.width) {
                            (Some(w), _) => Some(w),
                            (None, AvailableSpace::Definite(w)) => Some(w),
                            (None, AvailableSpace::MinContent) => {
                                Some(measurer.min_text_width(run))
                            }
                            (None, AvailableSpace::MaxContent) => None,
                        };
                        let (w, h) =
                            measurer.measure_text(run, if run.base.wrap { max } else { None });
                        Size {
                            width: known.width.unwrap_or(w),
                            height: known.height.unwrap_or(h),
                        }
                    }
                    Some(Leaf::Image(src)) => {
                        let (iw, ih) = measurer.image_size(src).unwrap_or((0.0, 0.0));
                        match (known.width, known.height) {
                            (Some(w), Some(h)) => Size {
                                width: w,
                                height: h,
                            },
                            (Some(w), None) if iw > 0.0 => Size {
                                width: w,
                                height: w * ih / iw,
                            },
                            (None, Some(h)) if ih > 0.0 => Size {
                                width: h * iw / ih,
                                height: h,
                            },
                            _ => Size {
                                width: iw,
                                height: ih,
                            },
                        }
                    }
                    None => Size {
                        width: known.width.unwrap_or(0.0),
                        height: known.height.unwrap_or(0.0),
                    },
                },
            )
        },
    );
    finish(&mut b, root, 0.0, 0.0)
}

fn finish(b: &mut Builder<'_>, idx: usize, ox: f32, oy: f32) -> Laid {
    let l = *b.tree.layout(b.plans[idx].node).expect("laid out");
    let rect = RectF {
        x: ox + l.location.x,
        y: oy + l.location.y,
        w: l.size.width,
        h: l.size.height,
    };
    let content_rect = RectF {
        x: rect.x + l.padding.left + l.border.left,
        y: rect.y + l.padding.top + l.border.top,
        w: (rect.w - l.padding.left - l.padding.right - l.border.left - l.border.right).max(0.0),
        h: (rect.h - l.padding.top - l.padding.bottom - l.border.top - l.border.bottom).max(0.0),
    };
    let children_idx = std::mem::take(&mut b.plans[idx].children);
    let children = children_idx
        .into_iter()
        .map(|c| finish(b, c, rect.x, rect.y))
        .collect();
    let plan = &mut b.plans[idx];
    Laid {
        id: plan.id,
        rect,
        content_rect,
        content: std::mem::replace(&mut plan.content, Content::None),
        paint: std::mem::take(&mut plan.paint),
        interactive: plan.interactive,
        children,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::Op;

    struct Fake;
    impl Measurer for Fake {
        fn measure_text(&mut self, run: &TextRun, max_width: Option<f32>) -> (f32, f32) {
            let size = run.base.size;
            let w = run.text().chars().count() as f32 * size * 0.5;
            match max_width {
                Some(m) if w > m && m > 0.0 => (m, size * 1.2 * (w / m).ceil()),
                _ => (w, size * 1.2),
            }
        }
        fn min_text_width(&mut self, run: &TextRun) -> f32 {
            run.base.size * 2.0
        }
        fn image_size(&mut self, _: &str) -> Option<(f32, f32)> {
            Some((100.0, 50.0))
        }
    }

    fn op_set(id: NodeId, name: &str, v: PropValue) -> Op {
        Op::SetProp {
            id,
            name: name.into(),
            value: v,
        }
    }

    #[test]
    fn flex_column_with_text() {
        let mut s = Scene::new();
        let ops = vec![
            Op::Create {
                id: 1,
                kind: "view".into(),
            },
            op_set(
                1,
                "style",
                PropValue::Map(vec![
                    ("flexDirection".into(), "column".into()),
                    ("padding".into(), 10.0.into()),
                    ("gap".into(), 4.0.into()),
                    ("fontSize".into(), 20.0.into()),
                    ("color".into(), "#ff0000".into()),
                ]),
            ),
            Op::Insert {
                parent: ROOT,
                child: 1,
                before: None,
            },
            Op::Create {
                id: 2,
                kind: "text".into(),
            },
            Op::Insert {
                parent: 1,
                child: 2,
                before: None,
            },
            Op::Create {
                id: 3,
                kind: "#text".into(),
            },
            Op::SetText {
                id: 3,
                text: "abcd".into(),
            },
            Op::Insert {
                parent: 2,
                child: 3,
                before: None,
            },
            Op::Create {
                id: 4,
                kind: "image".into(),
            },
            op_set(4, "src", "a.png".into()),
            op_set(
                4,
                "style",
                PropValue::Map(vec![("width".into(), 40.0.into())]),
            ),
            Op::Insert {
                parent: 1,
                child: 4,
                before: None,
            },
        ];
        s.apply_all(ops, 0.0);
        let laid = layout(&s, 200.0, 100.0, &mut Fake, 0.0);
        assert_eq!(laid.rect.w, 200.0);
        let col = &laid.children[0];
        let text = &col.children[0];
        assert_eq!((text.rect.x, text.rect.y), (10.0, 10.0));
        assert_eq!(text.rect.h, 24.0);
        match &text.content {
            Content::Text(run) => {
                assert_eq!(run.text(), "abcd");
                assert_eq!(run.base.color, Color::parse("#ff0000").unwrap());
            }
            other => panic!("{other:?}"),
        }
        let img = &col.children[1];
        assert_eq!(img.rect.y, 10.0 + 24.0 + 4.0);
        assert_eq!((img.rect.w, img.rect.h), (40.0, 20.0));
    }

    #[test]
    fn svg_shapes_and_hit_test() {
        let mut s = Scene::new();
        s.apply_all(
            vec![
                Op::Create {
                    id: 1,
                    kind: "svg".into(),
                },
                op_set(1, "viewBox", "0 0 88 88".into()),
                op_set(1, "width", 88.0.into()),
                op_set(1, "onClick", true.into()),
                Op::Insert {
                    parent: ROOT,
                    child: 1,
                    before: None,
                },
                Op::Create {
                    id: 2,
                    kind: "circle".into(),
                },
                op_set(2, "cx", 44.0.into()),
                op_set(2, "r", 32.0.into()),
                op_set(2, "fill", "none".into()),
                op_set(2, "stroke", "currentColor".into()),
                op_set(2, "strokeDasharray", "10 20".into()),
                op_set(2, "transform", "rotate(150 44 44)".into()),
                Op::Insert {
                    parent: 1,
                    child: 2,
                    before: None,
                },
            ],
            0.0,
        );
        let laid = layout(&s, 200.0, 200.0, &mut Fake, 0.0);
        let svg = &laid.children[0];
        assert_eq!((svg.rect.w, svg.rect.h), (88.0, 88.0));
        match &svg.content {
            Content::Svg(svg) => {
                let c = &svg.shapes[0];
                assert_eq!(c.fill, None);
                assert_eq!(c.stroke, Some(Color::WHITE));
                assert_eq!(c.dash, vec![10.0, 20.0]);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(laid.hit_test(10.0, 10.0), Some(1));
        assert_eq!(laid.hit_test(150.0, 150.0), None);
    }
}
