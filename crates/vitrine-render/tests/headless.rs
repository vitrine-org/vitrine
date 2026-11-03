//! Renders a sample scene off-screen and checks the result.
//! The PNG is written to `target/render-tests/` for visual inspection.

use vitrine_render::compose::{render_offscreen, write_png};
use vitrine_render::gpu::Gpu;
use vitrine_render::image::DirAssets;
use vitrine_render::scene::{NodeId, Op, ROOT};
use vitrine_render::style::Transform2D;
use vitrine_render::value::PropValue;
use vitrine_render::widget::WidgetRenderer;

struct B {
    ops: Vec<Op>,
    next: NodeId,
}

impl B {
    fn el(&mut self, parent: NodeId, kind: &str, props: &[(&str, PropValue)]) -> NodeId {
        self.next += 1;
        let id = self.next;
        self.ops.push(Op::Create {
            id,
            kind: kind.into(),
        });
        for (k, v) in props {
            self.ops.push(Op::SetProp {
                id,
                name: (*k).into(),
                value: v.clone(),
            });
        }
        self.ops.push(Op::Insert {
            parent,
            child: id,
            before: None,
        });
        id
    }

    fn text(&mut self, parent: NodeId, s: &str) {
        self.next += 1;
        let id = self.next;
        self.ops.push(Op::Create {
            id,
            kind: "#text".into(),
        });
        self.ops.push(Op::SetText { id, text: s.into() });
        self.ops.push(Op::Insert {
            parent,
            child: id,
            before: None,
        });
    }
}

fn style(pairs: &[(&str, PropValue)]) -> PropValue {
    PropValue::Map(
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
    )
}

#[test]
fn renders_sample_widget() {
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_MULTITHREADED,
        );
    }
    let gpu = Gpu::new(false).expect("gpu");
    let dc = gpu.new_context().unwrap();
    let mut w = WidgetRenderer::new(&gpu, Box::new(DirAssets(".".into())), &[]);

    let mut b = B {
        ops: vec![],
        next: 0,
    };
    let card = b.el(
        ROOT,
        "view",
        &[(
            "style",
            style(&[
                ("width", "100%".into()),
                ("height", "100%".into()),
                ("padding", 20.0.into()),
                ("gap", 12.0.into()),
                ("flexDirection", "column".into()),
                ("borderRadius", 16.0.into()),
                (
                    "background",
                    "linear-gradient(135deg, rgba(61,90,254,0.85), rgba(124,58,237,0.85))".into(),
                ),
                ("border", "1px solid rgba(255,255,255,0.35)".into()),
                ("color", "#ffffff".into()),
                ("fontFamily", "Segoe UI Variable Display".into()),
            ]),
        )],
    );
    let title = b.el(
        card,
        "text",
        &[(
            "style",
            style(&[("fontSize", 40.0.into()), ("fontWeight", 300.0.into())]),
        )],
    );
    b.text(title, "21");
    let accent = b.el(
        title,
        "span",
        &[("style", style(&[("color", "#FFC857".into())]))],
    );
    b.text(accent, ":42");
    let sub = b.el(
        card,
        "text",
        &[(
            "style",
            style(&[
                ("fontSize", 12.0.into()),
                ("letterSpacing", 2.0.into()),
                ("opacity", 0.8.into()),
            ]),
        )],
    );
    b.text(sub, "TUESDAY · 3 NOVEMBER");
    let row = b.el(
        card,
        "view",
        &[(
            "style",
            style(&[
                ("flexDirection", "row".into()),
                ("gap", 12.0.into()),
                ("alignItems", "center".into()),
            ]),
        )],
    );
    let svg = b.el(
        row,
        "svg",
        &[("viewBox", "0 0 48 48".into()), ("width", 48.0.into())],
    );
    b.el(
        svg,
        "circle",
        &[
            ("cx", 24.0.into()),
            ("cy", 24.0.into()),
            ("r", 20.0.into()),
            ("fill", "none".into()),
            ("stroke", "rgba(255,255,255,0.25)".into()),
            ("strokeWidth", 6.0.into()),
        ],
    );
    b.el(
        svg,
        "circle",
        &[
            ("cx", 24.0.into()),
            ("cy", 24.0.into()),
            ("r", 20.0.into()),
            ("fill", "none".into()),
            ("stroke", "#FFC857".into()),
            ("strokeWidth", 6.0.into()),
            ("strokeLinecap", "round".into()),
            ("strokeDasharray", "80 126".into()),
            ("transform", "rotate(-90 24 24)".into()),
        ],
    );
    b.el(
        svg,
        "path",
        &[
            ("d", "M18 26l4 4 8-10".into()),
            ("fill", "none".into()),
            ("stroke", "#fff".into()),
            ("strokeWidth", 3.0.into()),
            ("strokeLinecap", "round".into()),
            ("strokeLinejoin", "round".into()),
        ],
    );
    let label = b.el(
        row,
        "text",
        &[(
            "style",
            style(&[("fontSize", 14.0.into()), ("fontWeight", 600.0.into())]),
        )],
    );
    b.text(label, "CPU 64%");

    w.apply(b.ops, 0.0);
    let (width, height, scale) = (320.0f32, 200.0f32, 1.5f32);
    w.set_size(width, height);
    let (pw, ph) = ((width * scale) as u32, (height * scale) as u32);
    let rgba = render_offscreen(&dc, pw, ph, |dc| {
        w.paint(&gpu, dc, Transform2D::scale(scale, scale), 0.0)
    })
    .unwrap();

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/render-tests");
    std::fs::create_dir_all(&dir).unwrap();
    write_png(&dir.join("sample-widget.png"), pw, ph, &rgba).unwrap();

    let px = |x: u32, y: u32| &rgba[((y * pw + x) * 4) as usize..((y * pw + x) * 4 + 4) as usize];
    assert_eq!(px(0, 0)[3], 0, "rounded corner must stay transparent");
    assert!(
        px(pw / 2, ph - 10)[3] > 150,
        "card body must be mostly opaque"
    );
    // Some text pixels must be bright white-ish near the title.
    let bright = (0..pw)
        .flat_map(|x| (40..100).map(move |y| (x, y)))
        .filter(|&(x, y)| px(x, y)[0] > 240 && px(x, y)[1] > 240)
        .count();
    assert!(bright > 50, "title text should be painted ({bright})");
    // The layout tree exists and the svg is 48 DIPs wide.
    let laid = w.layout_tree().unwrap();
    assert_eq!(laid.children[0].children[2].children[0].rect.w, 48.0);
}
