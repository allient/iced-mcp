//! Walks a widget tree with an [`Operation`] and records what each widget
//! reports: the snapshot an agent reads instead of a screenshot.
//!
//! Widgets report themselves through `operate` — containers, scrollables,
//! focusables, text inputs and text. Visibility follows `iced_selector`:
//! scrollables translate and clip what is inside them.

use iced::advanced::text::paragraph::Plain;
use iced::advanced::text::Paragraph;
use iced::advanced::widget::operation::{Focusable, Scrollable, TextInput};
use iced::advanced::widget::{tree, Id, Operation, Tree};
use iced::{Rectangle, Vector};
use iced_mcp_protocol::{
    semantics_version, Layer, Node, NodeKind, Rect, ScrollInfo, SemanticNode, Size, TextFit,
};

use std::any::Any;

/// Text longer than this is cut, so one paragraph cannot flood a snapshot.
const MAX_TEXT: usize = 200;
/// Semantic nodes kept per widget: a chart describing a year of bars must
/// not turn one snapshot into megabytes.
const MAX_SEMANTIC_NODES: usize = 2000;

pub(crate) struct Collector {
    pub(crate) nodes: Vec<Node>,
    layer: Layer,
    viewport: Rectangle,
    translation: Vector,
    parent: Option<usize>,
    /// The last container recorded at this level: `traverse` makes it the
    /// parent of whatever its children report.
    pending: Option<usize>,
    /// A scrollable narrows the viewport before `traverse` enters its
    /// children. Restore the viewport that was active before that scrollable
    /// once the child traversal finishes, so following siblings are not
    /// treated as clipped by the scrollable.
    scroll_scope: Option<(Rectangle, Vector, Option<usize>)>,
    stack: Vec<(Rectangle, Vector, Option<usize>)>,
}

impl Collector {
    pub(crate) fn new(layer: Layer) -> Self {
        Self {
            nodes: Vec::new(),
            layer,
            viewport: Rectangle::INFINITE,
            translation: Vector::ZERO,
            parent: None,
            pending: None,
            scroll_scope: None,
            stack: Vec::new(),
        }
    }

    fn push(&mut self, kind: NodeKind, id: Option<&Id>, bounds: Rectangle) -> usize {
        let visible = self.viewport.intersection(&(bounds + self.translation));
        let index = self.nodes.len();
        self.nodes.push(Node {
            index,
            parent: self.parent,
            kind,
            layer: self.layer,
            id: id.and_then(id_name),
            text: None,
            bounds: rect(bounds + self.translation),
            visible: visible
                .filter(|v| v.width > 0.0 && v.height > 0.0)
                .map(rect),
            focused: None,
            interaction: None,
            scroll: None,
            role: None,
            value: None,
            label: None,
            description: None,
            placeholder: None,
            disabled: None,
            checked: None,
            indeterminate: None,
            selected: None,
            expanded: None,
            choices: None,
            numeric_value: None,
            min_numeric_value: None,
            max_numeric_value: None,
            numeric_value_step: None,
            track_bounds: None,
            vertical: None,
            inverted: None,
            text_fit: None,
        });
        index
    }
}

impl Collector {
    fn semantic(&mut self, node: &SemanticNode, parent: usize, budget: &mut usize) {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        let index = self.semantic_node(&node.role, &node.name, node.bounds, parent, Some(node));
        for child in &node.children {
            self.semantic(child, index, budget);
        }
    }

    /// A described node. Its bounds are already in window coordinates; the
    /// scroll offset still applies, as for any widget inside a scrollable.
    fn semantic_node(
        &mut self,
        role: &str,
        name: &str,
        bounds: Rect,
        parent: usize,
        semantics: Option<&SemanticNode>,
    ) -> usize {
        let value = semantics.and_then(|node| {
            node.value.as_ref().map(|value| match value {
                serde_json::Value::String(text) => text.clone(),
                other => other.to_string(),
            })
        });
        let r = Rectangle::new(
            iced::Point::new(bounds.x, bounds.y),
            iced::Size::new(bounds.width, bounds.height),
        );
        let visible = self.viewport.intersection(&(r + self.translation));
        let track_bounds = semantics.and_then(|node| {
            node.track_bounds.map(|bounds| {
                let track = Rectangle::new(
                    iced::Point::new(bounds.x, bounds.y),
                    iced::Size::new(bounds.width, bounds.height),
                );
                rect(track + self.translation)
            })
        });
        let index = self.nodes.len();
        self.nodes.push(Node {
            index,
            parent: Some(parent),
            kind: NodeKind::Semantic,
            layer: self.layer,
            id: semantics.and_then(|node| node.id.clone()),
            text: Some(clip(name)),
            bounds: rect(r + self.translation),
            visible: visible
                .filter(|v| v.width > 0.0 && v.height > 0.0)
                .map(rect),
            focused: None,
            interaction: None,
            scroll: None,
            role: Some(role.to_owned()),
            value: value.map(|v| clip(&v)),
            label: Some(clip(name)),
            description: semantics.and_then(|node| node.description.as_deref().map(clip)),
            placeholder: semantics.and_then(|node| node.placeholder.as_deref().map(clip)),
            disabled: semantics.and_then(|node| node.disabled),
            checked: semantics.and_then(|node| node.checked),
            indeterminate: semantics.and_then(|node| node.indeterminate),
            selected: semantics.and_then(|node| node.selected),
            expanded: semantics.and_then(|node| node.expanded),
            choices: semantics.and_then(|node| node.choices.clone()),
            numeric_value: semantics.and_then(|node| node.numeric_value),
            min_numeric_value: semantics.and_then(|node| node.min_numeric_value),
            max_numeric_value: semantics.and_then(|node| node.max_numeric_value),
            numeric_value_step: semantics.and_then(|node| node.numeric_value_step),
            track_bounds,
            vertical: semantics.and_then(|node| node.vertical),
            inverted: semantics.and_then(|node| node.inverted),
            text_fit: None,
        });
        index
    }
}

impl Operation for Collector {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        let parent = self.pending.take().or(self.parent);
        self.stack.push(self.scroll_scope.take().unwrap_or((
            self.viewport,
            self.translation,
            self.parent,
        )));
        self.parent = parent;
        operate(self);
        if let Some((viewport, translation, parent)) = self.stack.pop() {
            self.viewport = viewport;
            self.translation = translation;
            self.parent = parent;
        }
        self.pending = None;
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        let index = self.push(NodeKind::Container, id, bounds);
        self.pending = Some(index);
    }

    fn scrollable(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        content_bounds: Rectangle,
        translation: Vector,
        _state: &mut dyn Scrollable,
    ) {
        let index = self.push(NodeKind::Scrollable, id, bounds);
        if let Some(node) = self.nodes.get_mut(index) {
            node.scroll = Some(ScrollInfo {
                offset_x: translation.x,
                offset_y: translation.y,
                content_width: content_bounds.width,
                content_height: content_bounds.height,
            });
        }
        self.pending = Some(index);
        self.scroll_scope = Some((self.viewport, self.translation, self.parent));
        let visible = self.viewport.intersection(&(bounds + self.translation));
        self.translation -= translation;
        self.viewport = visible.unwrap_or_default();
    }

    fn focusable(&mut self, id: Option<&Id>, bounds: Rectangle, state: &mut dyn Focusable) {
        // A text input reports itself twice, input then focusable: fold the
        // focus into the node already recorded.
        if let Some(last) = self.nodes.last_mut() {
            let same_input = last.kind == NodeKind::TextInput
                && last.id == id.and_then(id_name)
                && last.bounds == rect(bounds + self.translation);
            if same_input {
                last.focused = Some(state.is_focused());
                return;
            }
        }
        let index = self.push(NodeKind::Focusable, id, bounds);
        if let Some(node) = self.nodes.get_mut(index) {
            node.focused = Some(state.is_focused());
        }
    }

    fn text_input(&mut self, id: Option<&Id>, bounds: Rectangle, state: &mut dyn TextInput) {
        let index = self.push(NodeKind::TextInput, id, bounds);
        if let Some(node) = self.nodes.get_mut(index) {
            node.text = Some(clip(state.text()));
        }
    }

    fn text(&mut self, id: Option<&Id>, bounds: Rectangle, text: &str) {
        let index = self.push(NodeKind::Text, id, bounds);
        if let Some(node) = self.nodes.get_mut(index) {
            node.text = Some(clip(text));
        }
    }

    fn custom(&mut self, id: Option<&Id>, bounds: Rectangle, state: &mut dyn Any) {
        // A `String` state is a widget's accessible label (the protocol's
        // `LABEL_CONVENTION`): what an icon-only button is called.
        if let Some(label) = state.downcast_ref::<String>() {
            let index = self.push(NodeKind::Label, id, bounds);
            if let Some(node) = self.nodes.get_mut(index) {
                node.text = Some(clip(label));
                node.label = Some(clip(label));
            }
            return;
        }
        // A `(SEMANTICS_MARKER, json)` state is what a custom-drawn widget
        // drew: list it under the widget.
        if let Some((marker, json)) = state.downcast_ref::<(&'static str, String)>() {
            if let Some(version) = semantics_version(marker) {
                let widget = self.push(NodeKind::Custom, id, bounds);
                let parsed = if version == 1 {
                    SemanticNode::parse(json)
                } else {
                    Err(format!(
                        "semantics version {version} is newer than this bridge (it reads 1): update iced-mcp"
                    ))
                };
                match parsed {
                    Ok(nodes) => {
                        let mut budget = MAX_SEMANTIC_NODES;
                        for node in &nodes {
                            self.semantic(node, widget, &mut budget);
                        }
                    }
                    Err(error) => {
                        // Shown in the snapshot, so the widget's author sees it.
                        self.semantic_node(
                            "error",
                            &format!("bad semantics: {error}"),
                            rect(bounds),
                            widget,
                            None,
                        );
                    }
                }
                return;
            }
        }
        // Other anonymous custom state (animations, caches) says nothing an
        // agent can use; one the app named is worth listing.
        if id.is_some() {
            self.push(NodeKind::Custom, id, bounds);
        }
    }
}

/// The name an app gave a widget with `Id::new("name")`; `None` for the
/// unique ids iced generates.
pub(crate) fn id_name(id: &Id) -> Option<String> {
    // `Id` keeps its variant private; its derived `Debug` is the only window.
    let debug = format!("{id:?}");
    let inner = debug.strip_prefix("Id(Custom(")?.strip_suffix("))")?;
    let name: String = serde_json::from_str(inner).ok()?;
    Some(name)
}

pub(crate) fn rect(r: Rectangle) -> Rect {
    Rect {
        x: r.x,
        y: r.y,
        width: r.width,
        height: r.height,
    }
}

fn clip(text: &str) -> String {
    if text.chars().count() <= MAX_TEXT {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(MAX_TEXT).collect();
    cut.push('…');
    cut
}

/// Measures every `text` widget's paragraph and records, on the text node
/// it reported, the size its text needs against the space it was given.
///
/// An [`Operation`] only hears a text's string and its (clamped) layout
/// bounds, which never show an overflow. The paragraph does: iced's `Text`
/// keeps a [`Plain`] paragraph in its widget state, laid out within the
/// limits the widget had. The tree is walked in the same pre-order as the
/// operation, so the n-th paragraph with some content belongs to the n-th
/// text node with that content. Widgets that keep their text elsewhere
/// (text inputs, rich text, custom widgets) are not measured.
pub(crate) fn measure_text<P: Paragraph + 'static>(tree: &Tree, nodes: &mut [Node]) {
    let mut found = Vec::new();
    paragraphs::<P>(tree, &mut found);
    assign_text_fit(found, nodes);
}

fn paragraphs<P: Paragraph + 'static>(tree: &Tree, out: &mut Vec<(String, TextFit)>) {
    if let tree::State::Some(state) = &tree.state {
        if let Some(plain) = state.downcast_ref::<Plain<P>>() {
            out.push((plain.content().to_owned(), text_fit(plain.raw())));
        }
    }
    for child in &tree.children {
        paragraphs::<P>(child, out);
    }
}

fn text_fit<P: Paragraph>(paragraph: &P) -> TextFit {
    let needed = paragraph.min_bounds();
    let available = paragraph.bounds();
    let finite = |v: f32| v.is_finite().then_some(v);
    TextFit {
        needed: Size {
            width: needed.width,
            height: needed.height,
        },
        available_width: finite(available.width),
        available_height: finite(available.height),
    }
}

/// Gives each measured paragraph to the first text node, in order, that
/// shows the same content and has no measurement yet.
pub(crate) fn assign_text_fit(found: Vec<(String, TextFit)>, nodes: &mut [Node]) {
    for (content, fit) in found {
        let text = clip(&content);
        if let Some(node) = nodes.iter_mut().find(|node| {
            node.kind == NodeKind::Text
                && node.text_fit.is_none()
                && node.text.as_deref() == Some(text.as_str())
        }) {
            node.text_fit = Some(fit);
        }
    }
}

/// The visible centre of every node worth asking "what cursor shows here":
/// the widgets an agent targets, not every layout row around them.
pub(crate) fn probe_points(nodes: &[Node]) -> Vec<(usize, iced::Point)> {
    let mut has_child = vec![false; nodes.len()];
    for node in nodes {
        if let Some(slot) = node.parent.and_then(|p| has_child.get_mut(p)) {
            *slot = true;
        }
    }
    nodes
        .iter()
        .zip(has_child)
        .filter_map(|(node, has_child)| {
            let visible = node.visible?;
            let wanted = match node.kind {
                NodeKind::Container => node.id.is_some() || !has_child,
                // Drawn content: the cursor is the canvas's own, and there
                // can be hundreds of them.
                NodeKind::Semantic => false,
                _ => true,
            };
            let (x, y) = visible.center();
            wanted.then_some((node.index, iced::Point::new(x, y)))
        })
        .collect()
}

/// What the grid need not hit-test again: every node already showing a
/// cursor, and the tight container around it — a button reports its label,
/// not its padding, yet the padding is the same button.
pub(crate) fn covered(nodes: &[Node]) -> Vec<Rect> {
    /// A parent this much larger than its child is layout, not a button.
    const MAX_GROWTH: f32 = 6.0;
    let mut rects = Vec::new();
    for node in nodes.iter().filter(|node| {
        node.interaction.is_some()
            // `label::labeled` describes an icon-only or custom control over
            // its full bounds. Treat that annotation as coverage so the
            // hit-test grid does not report the same control as anonymous.
            || node.kind == NodeKind::Label
            // Typed control semantics likewise identify their own target.
            || (node.kind == NodeKind::Semantic
                && matches!(
                    node.role.as_deref(),
                    Some(
                        "button"
                            | "checkbox"
                            | "combobox"
                            | "date"
                            | "menuitem"
                            | "menuitemcheckbox"
                            | "menuitemradio"
                            | "radio"
                            | "slider"
                            | "spinbutton"
                            | "switch"
                            | "tab"
                            | "tablist"
                            | "textbox"
                            | "time"
                    )
                ))
    }) {
        let Some(visible) = node.visible else {
            continue;
        };
        rects.push(visible);
        let parent = node.parent.and_then(|p| nodes.get(p));
        if let Some(parent) = parent.filter(|p| p.kind == NodeKind::Container) {
            if let Some(bounds) = parent.visible {
                if bounds.area() <= visible.area() * MAX_GROWTH {
                    rects.push(bounds);
                }
            }
        }
    }
    rects
}

/// Interactive areas no widget reported: hit-tests the centre of every
/// `step`-sized cell of `area`, skips cells inside `covered`, and merges runs
/// of cells showing the same cursor into rectangles. The cells are coarse, so
/// a region's edges are within a cell of the real ones.
pub(crate) fn regions(
    hit: &mut impl FnMut(iced::Point) -> iced::mouse::Interaction,
    area: Rectangle,
    step: f32,
    covered: &[Rect],
    layer: Layer,
    first_index: usize,
) -> Vec<Node> {
    /// Beyond this many cells the grid coarsens, bounding the cost.
    const MAX_CELLS: f32 = 40_000.0;
    let mut step = step.max(2.0);
    while (area.width / step) * (area.height / step) > MAX_CELLS {
        step *= 1.5;
    }
    let columns = (area.width / step).floor() as usize;
    let rows = (area.height / step).floor() as usize;

    // Open rectangles keyed by (first column, last column, cursor).
    let mut open: Vec<(usize, usize, String, Rect)> = Vec::new();
    let mut done: Vec<(String, Rect)> = Vec::new();
    for row in 0..rows {
        let y = area.y + (row as f32 + 0.5) * step;
        let mut runs: Vec<(usize, usize, String)> = Vec::new();
        for column in 0..columns {
            let x = area.x + (column as f32 + 0.5) * step;
            if covered.iter().any(|r| r.contains(x, y)) {
                continue;
            }
            let Some(name) = interaction_name(hit(iced::Point::new(x, y))) else {
                continue;
            };
            match runs.last_mut() {
                Some((_, end, last)) if *end + 1 == column && *last == name => *end = column,
                _ => runs.push((column, column, name)),
            }
        }
        let mut next_open = Vec::with_capacity(runs.len());
        for (start, end, name) in runs {
            let continued = open
                .iter()
                .position(|(s, e, n, _)| *s == start && *e == end && *n == name);
            let rect = match continued {
                Some(index) => {
                    let (_, _, _, mut rect) = open.swap_remove(index);
                    rect.height += step;
                    rect
                }
                None => Rect {
                    x: area.x + start as f32 * step,
                    y: area.y + row as f32 * step,
                    width: (end - start + 1) as f32 * step,
                    height: step,
                },
            };
            next_open.push((start, end, name, rect));
        }
        done.extend(open.drain(..).map(|(_, _, name, rect)| (name, rect)));
        open = next_open;
    }
    done.extend(open.into_iter().map(|(_, _, name, rect)| (name, rect)));
    // A strip one cell thick is the edge of something already reported (a
    // trigger's padding, a border), not a control of its own.
    done.retain(|(_, rect)| rect.width > step * 1.5 && rect.height > step * 1.5);
    done.sort_by(|a, b| a.1.y.total_cmp(&b.1.y).then(a.1.x.total_cmp(&b.1.x)));

    done.into_iter()
        .enumerate()
        .map(|(i, (name, rect))| Node {
            index: first_index + i,
            parent: None,
            kind: NodeKind::Region,
            layer,
            id: None,
            text: None,
            bounds: rect,
            visible: Some(rect),
            focused: None,
            interaction: Some(name),
            scroll: None,
            role: None,
            value: None,
            label: None,
            description: None,
            placeholder: None,
            disabled: None,
            checked: None,
            indeterminate: None,
            selected: None,
            expanded: None,
            choices: None,
            numeric_value: None,
            min_numeric_value: None,
            max_numeric_value: None,
            numeric_value_step: None,
            track_bounds: None,
            vertical: None,
            inverted: None,
            text_fit: None,
        })
        .collect()
}

/// `Pointer` → `"pointer"`; `None` for the cursors that say nothing.
pub(crate) fn interaction_name(interaction: iced::mouse::Interaction) -> Option<String> {
    use iced::mouse::Interaction;
    match interaction {
        Interaction::None | Interaction::Idle | Interaction::Hidden => None,
        other => Some(snake(&format!("{other:?}"))),
    }
}

fn snake(camel: &str) -> String {
    let mut out = String::with_capacity(camel.len() + 4);
    for (i, c) in camel.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_ids_keep_their_name() {
        assert_eq!(id_name(&Id::new("save")).as_deref(), Some("save"));
        assert_eq!(
            id_name(&Id::from(String::from("a \"quoted\" id"))).as_deref(),
            Some("a \"quoted\" id")
        );
        assert_eq!(id_name(&Id::unique()), None);
    }

    #[test]
    fn interactions_are_snake_case() {
        use iced::mouse::Interaction;
        assert_eq!(
            interaction_name(Interaction::Pointer).as_deref(),
            Some("pointer")
        );
        assert_eq!(
            interaction_name(Interaction::ResizingHorizontally).as_deref(),
            Some("resizing_horizontally")
        );
        assert_eq!(interaction_name(Interaction::Idle), None);
    }

    #[test]
    fn regions_merge_cells_into_rectangles_and_skip_covered_ones() {
        use iced::mouse::Interaction;
        // A pointer square at (20..60, 20..60), a text band at y 70..90 and a
        // one-cell pointer sliver at y 0..10.
        let mut hit = |p: iced::Point| {
            if (20.0..60.0).contains(&p.x) && (20.0..60.0).contains(&p.y) {
                Interaction::Pointer
            } else if (70.0..90.0).contains(&p.y) {
                Interaction::Text
            } else if p.y < 10.0 {
                Interaction::Pointer
            } else {
                Interaction::None
            }
        };
        let area = Rectangle::new(iced::Point::ORIGIN, iced::Size::new(100.0, 100.0));
        let found = regions(&mut hit, area, 10.0, &[], Layer::Base, 7);
        assert_eq!(found.len(), 2, "the sliver is dropped: {found:?}");
        assert_eq!(found[0].index, 7);
        assert_eq!(found[0].interaction.as_deref(), Some("pointer"));
        assert_eq!(
            found[0].bounds,
            Rect {
                x: 20.0,
                y: 20.0,
                width: 40.0,
                height: 40.0
            }
        );
        assert_eq!(found[1].interaction.as_deref(), Some("text"));
        assert_eq!(found[1].bounds.width, 100.0);

        let covered = [Rect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 65.0,
        }];
        let found = regions(&mut hit, area, 10.0, &covered, Layer::Base, 0);
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn a_string_custom_state_is_a_label() {
        let mut collector = Collector::new(Layer::Base);
        let bounds = Rectangle::new(iced::Point::new(1.0, 2.0), iced::Size::new(24.0, 24.0));
        Operation::custom(&mut collector, None, bounds, &mut String::from("Close"));
        Operation::custom(&mut collector, None, bounds, &mut 42_u32);
        assert_eq!(
            collector.nodes.len(),
            1,
            "anonymous non-label state is skipped"
        );
        assert_eq!(collector.nodes[0].kind, NodeKind::Label);
        assert_eq!(collector.nodes[0].text.as_deref(), Some("Close"));
    }

    #[test]
    fn semantics_become_nodes_under_their_widget() {
        let mut collector = Collector::new(Layer::Base);
        let bounds = Rectangle::new(iced::Point::new(0.0, 0.0), iced::Size::new(200.0, 100.0));
        let json = r#"[{"role":"bar","name":"12:45","value":{"o":1,"c":2},"bounds":{"x":10,"y":20,"width":5,"height":30},
            "children":[{"role":"handle","name":"top","bounds":{"x":10,"y":20,"width":2,"height":2}}]}]"#;
        let mut payload = (iced_mcp_protocol::SEMANTICS_MARKER, json.to_owned());
        Operation::custom(&mut collector, None, bounds, &mut payload);
        let nodes = &collector.nodes;
        assert_eq!(nodes.len(), 3, "widget, bar, handle");
        assert_eq!(nodes[1].kind, NodeKind::Semantic);
        assert_eq!(nodes[1].role.as_deref(), Some("bar"));
        assert_eq!(nodes[1].text.as_deref(), Some("12:45"));
        assert_eq!(nodes[1].value.as_deref(), Some(r#"{"c":2,"o":1}"#));
        assert_eq!(nodes[1].parent, Some(0));
        assert_eq!(nodes[2].parent, Some(1));

        let mut bad = (iced_mcp_protocol::SEMANTICS_MARKER, "{oops".to_owned());
        Operation::custom(&mut collector, None, bounds, &mut bad);
        let last = collector.nodes.last().unwrap();
        assert_eq!(last.role.as_deref(), Some("error"));
    }

    #[test]
    fn paragraphs_in_the_tree_measure_their_text_nodes() {
        use iced::advanced::text::{self, Text};
        type P = <iced::Renderer as text::Renderer>::Paragraph;
        let plain = |content: &str, width: f32| {
            Plain::<P>::new(Text {
                content: content.to_owned(),
                bounds: iced::Size::new(width, f32::INFINITY),
                size: iced::Pixels(16.0),
                line_height: text::LineHeight::default(),
                font: iced::Font::default(),
                align_x: text::Alignment::Default,
                align_y: iced::alignment::Vertical::Top,
                shaping: text::Shaping::Advanced,
                wrapping: text::Wrapping::None,
            })
        };
        let leaf = |state: Plain<P>| Tree {
            tag: tree::Tag::of::<Plain<P>>(),
            state: tree::State::new(state),
            children: Vec::new(),
        };
        let root = Tree {
            tag: tree::Tag::stateless(),
            state: tree::State::None,
            children: vec![
                leaf(plain("A label far too long for forty pixels", 40.0)),
                leaf(plain("Ok", 400.0)),
            ],
        };
        let mut collector = Collector::new(Layer::Base);
        let bounds = Rectangle::new(iced::Point::ORIGIN, iced::Size::new(40.0, 20.0));
        Operation::text(
            &mut collector,
            None,
            bounds,
            "A label far too long for forty pixels",
        );
        Operation::text(&mut collector, None, bounds, "Ok");
        Operation::text(&mut collector, None, bounds, "Never measured");
        measure_text::<P>(&root, &mut collector.nodes);
        let fits = |i: usize| collector.nodes[i].text_fit.map(|fit| fit.fits());
        assert_eq!(fits(0), Some(false), "{:?}", collector.nodes[0].text_fit);
        assert_eq!(fits(1), Some(true));
        assert_eq!(fits(2), None, "no paragraph: unknown");
    }

    #[test]
    fn measurements_go_to_matching_text_in_order() {
        let fit = |w| TextFit {
            needed: Size {
                width: w,
                height: 10.0,
            },
            available_width: Some(50.0),
            available_height: None,
        };
        let mut collector = Collector::new(Layer::Base);
        let bounds = Rectangle::new(iced::Point::ORIGIN, iced::Size::new(50.0, 10.0));
        for text in ["Add", "Save", "Add"] {
            Operation::text(&mut collector, None, bounds, text);
        }
        assign_text_fit(
            vec![
                ("Add".into(), fit(10.0)),
                ("Add".into(), fit(90.0)),
                ("Other".into(), fit(1.0)),
            ],
            &mut collector.nodes,
        );
        let widths: Vec<_> = collector
            .nodes
            .iter()
            .map(|n| n.text_fit.map(|f| f.needed.width))
            .collect();
        assert_eq!(widths, [Some(10.0), None, Some(90.0)]);
    }

    #[test]
    fn long_text_is_clipped() {
        let long = "x".repeat(MAX_TEXT + 10);
        assert_eq!(clip(&long).chars().count(), MAX_TEXT + 1);
    }
}
