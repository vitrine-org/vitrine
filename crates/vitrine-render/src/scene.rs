//! Retained node tree mutated by widget scripts through [`Op`]s.

use std::collections::HashMap;

use crate::anim::{Animations, parse_transitions};
use crate::value::PropValue;

pub type NodeId = u32;
pub const ROOT: NodeId = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    View,
    Text,
    TextNode,
    Br,
    Image,
    Svg,
    Group,
    Circle,
    Ellipse,
    Rect,
    Line,
    Path,
}

impl Kind {
    pub fn parse(tag: &str) -> Kind {
        match tag {
            "text" | "span" | "p" | "label" => Kind::Text,
            "#text" => Kind::TextNode,
            "br" => Kind::Br,
            "image" | "img" => Kind::Image,
            "svg" => Kind::Svg,
            "g" => Kind::Group,
            "circle" => Kind::Circle,
            "ellipse" => Kind::Ellipse,
            "rect" => Kind::Rect,
            "line" => Kind::Line,
            "path" => Kind::Path,
            _ => Kind::View,
        }
    }

    pub fn is_shape(self) -> bool {
        matches!(
            self,
            Kind::Group | Kind::Circle | Kind::Ellipse | Kind::Rect | Kind::Line | Kind::Path
        )
    }
}

#[derive(Clone, Debug)]
pub struct Node {
    pub kind: Kind,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub style: Vec<(String, PropValue)>,
    pub attrs: Vec<(String, PropValue)>,
    pub text: String,
    /// Has at least one pointer event handler on the script side.
    pub interactive: bool,
}

impl Node {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            parent: None,
            children: Vec::new(),
            style: Vec::new(),
            attrs: Vec::new(),
            text: String::new(),
            interactive: false,
        }
    }

    pub fn style(&self, key: &str) -> Option<&PropValue> {
        self.style.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn attr(&self, key: &str) -> Option<&PropValue> {
        self.attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Create {
        id: NodeId,
        kind: String,
    },
    SetProp {
        id: NodeId,
        name: String,
        value: PropValue,
    },
    SetText {
        id: NodeId,
        text: String,
    },
    Insert {
        parent: NodeId,
        child: NodeId,
        before: Option<NodeId>,
    },
    Remove {
        parent: NodeId,
        child: NodeId,
    },
    Destroy {
        id: NodeId,
    },
}

/// `font-size` → `fontSize`, `stroke-dasharray` → `strokeDasharray`.
pub fn camel_case(s: &str) -> String {
    if !s.contains('-') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut upper = false;
    for c in s.chars() {
        if c == '-' {
            upper = !out.is_empty();
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// Parses an inline CSS string `"color: red; font-size: 12px"`.
pub fn parse_inline_style(s: &str) -> Vec<(String, PropValue)> {
    s.split(';')
        .filter_map(|decl| {
            let (k, v) = decl.split_once(':')?;
            let k = camel_case(k.trim());
            (!k.is_empty()).then(|| (k, PropValue::String(v.trim().to_string())))
        })
        .collect()
}

pub struct Scene {
    nodes: HashMap<NodeId, Node>,
    pub anims: Animations,
    /// Structure, text or style changed since the last layout.
    pub layout_dirty: bool,
    /// Something visible changed since the last paint.
    pub paint_dirty: bool,
}

impl Default for Scene {
    fn default() -> Self {
        Self::new()
    }
}

impl Scene {
    pub fn new() -> Self {
        let mut root = Node::new(Kind::View);
        root.style = vec![
            ("width".into(), "100%".into()),
            ("height".into(), "100%".into()),
        ];
        let mut nodes = HashMap::new();
        nodes.insert(ROOT, root);
        Self {
            nodes,
            anims: Animations::default(),
            layout_dirty: true,
            paint_dirty: true,
        }
    }

    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(&id)
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.len() <= 1
    }

    /// The effective style value, taking running transitions into account.
    pub fn style_value(&self, id: NodeId, key: &str, now: f64) -> Option<PropValue> {
        let node = self.nodes.get(&id)?;
        let declared = node.style(key)?;
        Some(self.animated(id, &format!("s:{key}"), declared, now))
    }

    /// The effective attribute value, taking running transitions into account.
    pub fn attr_value(&self, id: NodeId, key: &str, now: f64) -> Option<PropValue> {
        let node = self.nodes.get(&id)?;
        let declared = node.attr(key)?;
        Some(self.animated(id, &format!("a:{key}"), declared, now))
    }

    fn animated(&self, id: NodeId, key: &str, declared: &PropValue, now: f64) -> PropValue {
        match self.anims.value(id, key, now) {
            Some(v) if v.len() == 1 && !matches!(declared, PropValue::List(_)) => {
                PropValue::Number(v[0] as f64)
            }
            Some(v) => {
                PropValue::List(v.into_iter().map(|x| PropValue::Number(x as f64)).collect())
            }
            None => declared.clone(),
        }
    }

    /// Applies a batch of operations. `now` is a monotonic time in milliseconds.
    pub fn apply_all(&mut self, ops: impl IntoIterator<Item = Op>, now: f64) {
        for op in ops {
            self.apply(op, now);
        }
    }

    pub fn apply(&mut self, op: Op, now: f64) {
        match op {
            Op::Create { id, kind } => {
                if id != ROOT {
                    self.nodes.insert(id, Node::new(Kind::parse(&kind)));
                }
            }
            Op::SetText { id, text } => {
                if let Some(n) = self.nodes.get_mut(&id) {
                    if n.text != text {
                        n.text = text;
                        self.layout_dirty = true;
                    }
                }
            }
            Op::SetProp { id, name, value } => self.set_prop(id, &name, value, now),
            Op::Insert {
                parent,
                child,
                before,
            } => {
                if parent == child
                    || !self.nodes.contains_key(&parent)
                    || !self.nodes.contains_key(&child)
                {
                    return;
                }
                self.detach(child);
                let p = self.nodes.get_mut(&parent).unwrap();
                let idx = before
                    .and_then(|b| p.children.iter().position(|c| *c == b))
                    .unwrap_or(p.children.len());
                p.children.insert(idx, child);
                self.nodes.get_mut(&child).unwrap().parent = Some(parent);
                self.layout_dirty = true;
            }
            Op::Remove { parent, child } => {
                if self.nodes.get(&child).and_then(|c| c.parent) == Some(parent) {
                    self.detach(child);
                    self.layout_dirty = true;
                }
            }
            Op::Destroy { id } => {
                if id == ROOT {
                    return;
                }
                self.detach(id);
                if let Some(node) = self.nodes.remove(&id) {
                    for c in node.children {
                        if let Some(cn) = self.nodes.get_mut(&c) {
                            cn.parent = None;
                        }
                    }
                }
                self.anims.remove_node(id);
                self.layout_dirty = true;
            }
        }
    }

    fn detach(&mut self, child: NodeId) {
        let Some(old) = self.nodes.get(&child).and_then(|c| c.parent) else {
            return;
        };
        if let Some(p) = self.nodes.get_mut(&old) {
            p.children.retain(|c| *c != child);
        }
        if let Some(c) = self.nodes.get_mut(&child) {
            c.parent = None;
        }
    }

    fn set_prop(&mut self, id: NodeId, name: &str, value: PropValue, now: f64) {
        if !self.nodes.contains_key(&id) {
            return;
        }
        let name = camel_case(name);
        if name.starts_with("on") && name.len() > 2 && name.as_bytes()[2].is_ascii_uppercase() {
            self.nodes.get_mut(&id).unwrap().interactive = value.as_bool();
            return;
        }
        match name.as_str() {
            "style" => {
                let new_style: Vec<(String, PropValue)> = match value {
                    PropValue::Map(entries) => entries
                        .into_iter()
                        .map(|(k, v)| (camel_case(&k), v))
                        .collect(),
                    PropValue::String(s) => parse_inline_style(&s),
                    _ => Vec::new(),
                };
                let specs = new_style
                    .iter()
                    .find(|(k, _)| k == "transition")
                    .map(|(_, v)| parse_transitions(&v.to_string()))
                    .unwrap_or_default();
                for spec in &specs {
                    for (k, v) in &new_style {
                        if spec.prop == *k || spec.prop == "all" {
                            let from = self
                                .style_value(id, k, now)
                                .map(|p| p.as_numbers())
                                .unwrap_or_default();
                            self.anims
                                .start(id, format!("s:{k}"), from, v.as_numbers(), spec, now);
                        }
                    }
                }
                let node = self.nodes.get_mut(&id).unwrap();
                if node.style != new_style {
                    node.style = new_style;
                    self.layout_dirty = true;
                }
            }
            "class" | "className" | "ref" | "key" | "children" => {}
            _ => {
                let transition = self
                    .nodes
                    .get(&id)
                    .and_then(|n| n.style("transition").or_else(|| n.attr("transition")))
                    .map(|v| parse_transitions(&v.to_string()))
                    .unwrap_or_default();
                if let Some(spec) = transition
                    .iter()
                    .find(|s| s.prop == name || s.prop == "all")
                {
                    let from = self
                        .attr_value(id, &name, now)
                        .map(|p| p.as_numbers())
                        .unwrap_or_default();
                    let spec = spec.clone();
                    self.anims.start(
                        id,
                        format!("a:{name}"),
                        from,
                        value.as_numbers(),
                        &spec,
                        now,
                    );
                }
                let node = self.nodes.get_mut(&id).unwrap();
                let layout_attr = matches!(name.as_str(), "src" | "width" | "height" | "viewBox")
                    && !node.kind.is_shape();
                match node.attrs.iter_mut().find(|(k, _)| *k == name) {
                    Some((_, v)) if *v == value => return,
                    Some((_, v)) => *v = value,
                    None => node.attrs.push((name, value)),
                }
                if layout_attr {
                    self.layout_dirty = true;
                } else {
                    self.paint_dirty = true;
                }
            }
        }
    }

    /// Whether another frame is needed to advance transitions.
    pub fn animating(&self, now: f64) -> bool {
        self.anims.is_active(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create(s: &mut Scene, id: NodeId, kind: &str) {
        s.apply(
            Op::Create {
                id,
                kind: kind.into(),
            },
            0.0,
        );
    }

    #[test]
    fn tree_ops() {
        let mut s = Scene::new();
        create(&mut s, 1, "view");
        create(&mut s, 2, "text");
        create(&mut s, 3, "#text");
        s.apply(
            Op::Insert {
                parent: ROOT,
                child: 1,
                before: None,
            },
            0.0,
        );
        s.apply(
            Op::Insert {
                parent: 1,
                child: 2,
                before: None,
            },
            0.0,
        );
        s.apply(
            Op::Insert {
                parent: 2,
                child: 3,
                before: None,
            },
            0.0,
        );
        s.apply(
            Op::SetText {
                id: 3,
                text: "hi".into(),
            },
            0.0,
        );
        assert_eq!(s.node(1).unwrap().children, vec![2]);
        // move 2 before 1 at root level
        s.apply(
            Op::Insert {
                parent: ROOT,
                child: 2,
                before: Some(1),
            },
            0.0,
        );
        assert_eq!(s.node(ROOT).unwrap().children, vec![2, 1]);
        assert!(s.node(1).unwrap().children.is_empty());
        s.apply(
            Op::Remove {
                parent: ROOT,
                child: 2,
            },
            0.0,
        );
        s.apply(Op::Destroy { id: 2 }, 0.0);
        assert!(s.node(2).is_none());
        assert_eq!(s.node(3).unwrap().parent, None);
    }

    #[test]
    fn style_and_transitions() {
        let mut s = Scene::new();
        create(&mut s, 1, "circle");
        s.apply(
            Op::SetProp {
                id: 1,
                name: "style".into(),
                value: "transition: stroke-dasharray 400ms linear".into(),
            },
            0.0,
        );
        s.apply(
            Op::SetProp {
                id: 1,
                name: "stroke-dasharray".into(),
                value: "0 100".into(),
            },
            0.0,
        );
        s.apply(
            Op::SetProp {
                id: 1,
                name: "strokeDasharray".into(),
                value: "100 0".into(),
            },
            1000.0,
        );
        let mid = s
            .attr_value(1, "strokeDasharray", 1200.0)
            .unwrap()
            .as_numbers();
        assert_eq!(mid, vec![50.0, 50.0]);
        assert!(s.animating(1200.0));
        assert_eq!(
            s.attr_value(1, "strokeDasharray", 2000.0)
                .unwrap()
                .as_numbers(),
            vec![100.0, 0.0]
        );
        s.apply(
            Op::SetProp {
                id: 1,
                name: "onClick".into(),
                value: true.into(),
            },
            0.0,
        );
        assert!(s.node(1).unwrap().interactive);
    }
}
