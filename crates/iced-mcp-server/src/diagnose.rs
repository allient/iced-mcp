//! Layout diagnostics: geometry mistakes a snapshot can show without a
//! screenshot — interactive widgets on top of each other, content sticking
//! out of its container or the window, text that does not fit, and ids used
//! twice.
//!
//! Every rule errs towards silence: a finding should be a bug worth a look,
//! so anything the snapshot cannot tell apart from a deliberate layout
//! (drawn canvas content, approximate hit-test regions, scrolled-out
//! content, an overlay above the base) is left out. The heuristics and their
//! limits are documented in `docs/guide.md` under *Layout diagnostics*.

use iced_mcp_protocol::{Node, NodeKind, Rect, Snapshot};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::view;

/// Edges within this many logical pixels count as touching, not crossing:
/// layout rounds to fractions of a pixel.
const SLACK: f32 = 1.0;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DiagnosticKind {
    /// Two interactive widgets cover the same pixels.
    Overlap,
    /// A widget sticks out of its container or out of the window.
    Clipped,
    /// A text needs more room than its widget was given.
    TextOverflow,
    /// The same widget id on two different widgets of one window.
    DuplicateId,
}

impl DiagnosticKind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Overlap => "overlap",
            Self::Clipped => "clipped",
            Self::TextOverflow => "text_overflow",
            Self::DuplicateId => "duplicate_id",
        }
    }
}

/// One finding: what, which nodes (refs into the snapshot), where, and why
/// it matters.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct Finding {
    pub(crate) kind: DiagnosticKind,
    pub(crate) refs: Vec<usize>,
    pub(crate) ids: Vec<Option<String>>,
    pub(crate) bounds: Vec<Rect>,
    pub(crate) explanation: String,
}

/// Every finding in `snapshot`, of the `kinds` asked for (all when empty),
/// in a stable order: by kind, then by first ref.
pub(crate) fn diagnose(snapshot: &Snapshot, kinds: &[DiagnosticKind]) -> Vec<Finding> {
    let wanted = |kind: DiagnosticKind| kinds.is_empty() || kinds.contains(&kind);
    let tree = Tree::new(&snapshot.nodes);
    let mut findings = Vec::new();
    if wanted(DiagnosticKind::Overlap) {
        overlaps(&tree, &mut findings);
    }
    if wanted(DiagnosticKind::Clipped) {
        clipped(&tree, snapshot, &mut findings);
    }
    if wanted(DiagnosticKind::TextOverflow) {
        text_overflow(&tree, &mut findings);
    }
    if wanted(DiagnosticKind::DuplicateId) {
        duplicate_ids(&tree, &mut findings);
    }
    findings.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.refs.cmp(&b.refs)));
    findings
}

struct Tree<'a> {
    nodes: &'a [Node],
    has_child: Vec<bool>,
}

impl<'a> Tree<'a> {
    fn new(nodes: &'a [Node]) -> Self {
        let mut has_child = vec![false; nodes.len()];
        for node in nodes {
            if let Some(slot) = node.parent.and_then(|p| has_child.get_mut(p)) {
                *slot = true;
            }
        }
        Self { nodes, has_child }
    }

    fn parent(&self, node: &Node) -> Option<&'a Node> {
        node.parent.and_then(|p| self.nodes.get(p))
    }

    fn is_ancestor(&self, ancestor: usize, node: &Node) -> bool {
        std::iter::successors(node.parent, |&p| self.nodes.get(p).and_then(|n| n.parent))
            .any(|p| p == ancestor)
    }
}

fn finding(kind: DiagnosticKind, nodes: &[&Node], explanation: String) -> Finding {
    Finding {
        kind,
        refs: nodes.iter().map(|n| n.index).collect(),
        ids: nodes.iter().map(|n| n.id.clone()).collect(),
        bounds: nodes
            .iter()
            .map(|n| n.visible.unwrap_or(n.bounds))
            .collect(),
        explanation,
    }
}

fn intersection(a: &Rect, b: &Rect) -> Option<Rect> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = (a.x + a.width).min(b.x + b.width);
    let bottom = (a.y + a.height).min(b.y + b.height);
    (right > x && bottom > y).then_some(Rect {
        x,
        y,
        width: right - x,
        height: bottom - y,
    })
}

/// Interactive leaves (what a click lands on) whose visible areas overlap.
/// Ancestors and descendants, labels (which name the control they cover),
/// approximate regions, drawn content and overlay-over-base pairs are
/// expected to overlap and are skipped.
fn overlaps(tree: &Tree<'_>, findings: &mut Vec<Finding>) {
    let candidates: Vec<(&Node, Rect)> = tree
        .nodes
        .iter()
        .filter(|n| {
            n.interaction.is_some()
                && !tree.has_child[n.index]
                && !matches!(
                    n.kind,
                    NodeKind::Label | NodeKind::Region | NodeKind::Semantic | NodeKind::Scrollable
                )
        })
        .filter_map(|n| n.visible.map(|v| (n, v)))
        .collect();
    for (i, (a, va)) in candidates.iter().enumerate() {
        for (b, vb) in &candidates[i + 1..] {
            if a.layer != b.layer
                || tree.is_ancestor(a.index, b)
                || tree.is_ancestor(b.index, a)
                // The same thing drawn twice (listed once by snapshot).
                || (a.text == b.text && va == vb)
            {
                continue;
            }
            let Some(overlap) = intersection(va, vb) else {
                continue;
            };
            if overlap.width <= SLACK || overlap.height <= SLACK {
                continue;
            }
            findings.push(finding(
                DiagnosticKind::Overlap,
                &[a, b],
                format!(
                    "{} and {} overlap by {:.0}×{:.0} px at ({:.0},{:.0}): a click there reaches only one of them",
                    short(a),
                    short(b),
                    overlap.width,
                    overlap.height,
                    overlap.x,
                    overlap.y
                ),
            ));
        }
    }
}

/// How far `inner` sticks out of `outer` on each side, as text, or `None`
/// when it stays within `SLACK`.
fn overhang(inner: &Rect, outer: &Rect) -> Option<String> {
    let sides = [
        ("left", outer.x - inner.x),
        ("top", outer.y - inner.y),
        ("right", (inner.x + inner.width) - (outer.x + outer.width)),
        (
            "bottom",
            (inner.y + inner.height) - (outer.y + outer.height),
        ),
    ];
    let out: Vec<String> = sides
        .iter()
        .filter(|(_, by)| *by > SLACK)
        .map(|(side, by)| format!("{by:.0} px past the {side}"))
        .collect();
    (!out.is_empty()).then(|| out.join(", "))
}

/// Whether layout squeezed `node` to nothing: iced shrinks what does not
/// fit to zero width or height rather than letting it overflow, so a
/// control with an empty box is one the window ran out of room for.
/// (Scrolled-out nodes keep their size; only their visible part is empty.)
fn squeezed(node: &Node) -> bool {
    let shows_something = match node.kind {
        NodeKind::TextInput | NodeKind::Focusable | NodeKind::Label => true,
        // Measured text is judged by `text_overflow`.
        NodeKind::Text => {
            node.text_fit.is_none() && node.text.as_deref().is_some_and(|t| !t.trim().is_empty())
        }
        _ => false,
    };
    shows_something && (node.bounds.width < SLACK || node.bounds.height < SLACK)
}

/// Content larger than its container (the nearest reported layout box),
/// reaching past the window, or squeezed to nothing. Children of a
/// scrollable are its scrolled content, and nodes scrolled out of view are
/// hidden, so neither counts. Only the outermost node that crosses the
/// window edge is reported.
fn clipped(tree: &Tree<'_>, snapshot: &Snapshot, findings: &mut Vec<Finding>) {
    for node in tree.nodes.iter().filter(|n| squeezed(n)) {
        findings.push(finding(
            DiagnosticKind::Clipped,
            &[node],
            format!(
                "{} was squeezed to {:.0}×{:.0}: the layout ran out of room for it",
                short(node),
                node.bounds.width,
                node.bounds.height
            ),
        ));
    }
    let window = Rect {
        x: 0.0,
        y: 0.0,
        width: snapshot.size.width,
        height: snapshot.size.height,
    };
    let checked =
        |n: &Node| n.visible.is_some() && !matches!(n.kind, NodeKind::Semantic | NodeKind::Region);
    let mut past_window = vec![false; tree.nodes.len()];
    for node in tree.nodes.iter().filter(|n| checked(n)) {
        if let Some(parent) = tree
            .parent(node)
            .filter(|p| p.kind == NodeKind::Container && p.layer == node.layer)
        {
            if let Some(how) = overhang(&node.bounds, &parent.bounds) {
                findings.push(finding(
                    DiagnosticKind::Clipped,
                    &[node, parent],
                    format!(
                        "{} is larger than its container {}: {how}",
                        short(node),
                        short(parent)
                    ),
                ));
                continue;
            }
        }
        let Some(visible) = node.visible else {
            continue;
        };
        if window.width <= 0.0 || window.height <= 0.0 {
            continue;
        }
        if let Some(how) = overhang(&visible, &window) {
            past_window[node.index] = true;
            let parent_past = node.parent.is_some_and(|p| past_window[p]);
            if !parent_past {
                findings.push(finding(
                    DiagnosticKind::Clipped,
                    &[node],
                    format!(
                        "{} reaches outside the {:.0}×{:.0} window: {how}",
                        short(node),
                        window.width,
                        window.height
                    ),
                ));
            }
        }
    }
}

/// Text whose paragraph needs more room than its widget had: cut off or
/// drawn over its neighbours. Only text the bridge measured (iced's `text`
/// widget in the base layer) can be judged.
fn text_overflow(tree: &Tree<'_>, findings: &mut Vec<Finding>) {
    // Scrolled out: not on screen. Squeezed to an empty box: on screen as
    // nothing, which is exactly what this should report.
    let on_screen = |n: &Node| n.visible.is_some() || n.bounds.area() < SLACK;
    for node in tree.nodes.iter().filter(|n| on_screen(n)) {
        let Some(fit) = node.text_fit.filter(|fit| !fit.fits()) else {
            continue;
        };
        let mut why = Vec::new();
        if let Some(width) = fit
            .available_width
            .filter(|w| fit.needed.width > w + SLACK / 2.0)
        {
            why.push(format!(
                "needs {:.0} px of width but has {width:.0} (no wrapping, or a word longer than the line)",
                fit.needed.width
            ));
        }
        if let Some(height) = fit
            .available_height
            .filter(|h| fit.needed.height > h + SLACK / 2.0)
        {
            why.push(format!(
                "needs {:.0} px of height but has {height:.0} (a fixed height too small for its lines, or the layout squeezed it)",
                fit.needed.height
            ));
        }
        findings.push(finding(
            DiagnosticKind::TextOverflow,
            &[node],
            format!("{} does not fit: {}", short(node), why.join("; ")),
        ));
    }
}

/// An id given to two widgets with different bounds: `#id` targets and
/// `widget::Id` operations reach only one of them. The same widget
/// reporting itself twice (same box) is not a duplicate.
fn duplicate_ids(tree: &Tree<'_>, findings: &mut Vec<Finding>) {
    let mut by_id: BTreeMap<&str, Vec<&Node>> = BTreeMap::new();
    for node in tree.nodes {
        if let Some(id) = node.id.as_deref() {
            let same = by_id.entry(id).or_default();
            if !same.iter().any(|n| n.bounds == node.bounds) {
                same.push(node);
            }
        }
    }
    for (id, nodes) in by_id {
        if nodes.len() > 1 {
            findings.push(finding(
                DiagnosticKind::DuplicateId,
                &nodes,
                format!(
                    "#{id} is on {} widgets: `#{id}` targets and widget operations reach only the first",
                    nodes.len()
                ),
            ));
        }
    }
}

/// `[5] text "Add"`, `[9] box #sidebar`: enough to recognise a node.
fn short(node: &Node) -> String {
    let line = view::line(node);
    // `[5] text "Add" #id (x,y w×h) …` → cut before the geometry.
    match line.find(" (") {
        Some(cut) => line[..cut].to_owned(),
        None => line,
    }
}

/// The findings as an agent reads them.
pub(crate) fn render(window: &str, findings: &[Finding], visible: usize) -> String {
    if findings.is_empty() {
        return format!(
            "No layout findings in window {window} ({visible} visible nodes). This checks what the widget tree reports; see the guide for what it cannot see."
        );
    }
    let mut text = format!("{} layout finding(s) in window {window}:\n", findings.len());
    for f in findings {
        let boxes: Vec<String> = f.bounds.iter().map(view::rect_text).collect();
        let _ = writeln!(
            text,
            "- {}: {} {}",
            f.kind.name(),
            f.explanation,
            boxes.join(" ")
        );
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_mcp_protocol::{Layer, Size, TextFit};

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn node(index: usize, parent: Option<usize>, kind: NodeKind, r: Rect) -> Node {
        let mut node: Node = serde_json::from_value(serde_json::json!({
            "index": index,
            "kind": "text",
            "layer": "base",
            "bounds": {"x": 0, "y": 0, "width": 0, "height": 0},
        }))
        .unwrap();
        node.parent = parent;
        node.kind = kind;
        node.bounds = r;
        node.visible = Some(r);
        node
    }

    fn snapshot(nodes: Vec<Node>) -> Snapshot {
        Snapshot {
            window: "1".into(),
            size: Size {
                width: 400.0,
                height: 300.0,
            },
            nodes,
        }
    }

    fn kinds(findings: &[Finding]) -> Vec<(DiagnosticKind, Vec<usize>)> {
        findings.iter().map(|f| (f.kind, f.refs.clone())).collect()
    }

    #[test]
    fn a_clean_layout_has_no_findings() {
        let root = node(0, None, NodeKind::Container, rect(0.0, 0.0, 400.0, 300.0));
        let mut add = node(1, Some(0), NodeKind::Text, rect(10.0, 10.0, 40.0, 20.0));
        add.interaction = Some("pointer".into());
        let mut save = node(2, Some(0), NodeKind::Text, rect(60.0, 10.0, 40.0, 20.0));
        save.interaction = Some("pointer".into());
        assert!(diagnose(&snapshot(vec![root, add, save]), &[]).is_empty());
    }

    #[test]
    fn overlapping_interactive_leaves_are_found_but_not_nesting_or_labels() {
        let root = node(0, None, NodeKind::Container, rect(0.0, 0.0, 400.0, 300.0));
        let mut a = node(1, Some(0), NodeKind::Text, rect(10.0, 10.0, 40.0, 20.0));
        a.interaction = Some("pointer".into());
        let mut b = node(2, Some(0), NodeKind::Text, rect(30.0, 15.0, 40.0, 20.0));
        b.interaction = Some("pointer".into());
        // A label over `a` names it; an inert text over it is not clickable.
        let mut label = node(3, Some(0), NodeKind::Label, rect(10.0, 10.0, 40.0, 20.0));
        label.interaction = Some("pointer".into());
        let inert = node(4, Some(0), NodeKind::Text, rect(10.0, 10.0, 40.0, 20.0));
        // An overlay menu over the base is expected.
        let mut menu = node(5, None, NodeKind::Text, rect(10.0, 10.0, 80.0, 80.0));
        menu.interaction = Some("pointer".into());
        menu.layer = Layer::Overlay;
        // Touching edges (within a pixel) are not an overlap.
        let mut c = node(6, Some(0), NodeKind::Text, rect(70.5, 15.0, 40.0, 20.0));
        c.interaction = Some("pointer".into());
        let findings = diagnose(&snapshot(vec![root, a, b, label, inert, menu, c]), &[]);
        assert_eq!(kinds(&findings), [(DiagnosticKind::Overlap, vec![1, 2])]);
        assert!(
            findings[0]
                .explanation
                .contains("overlap by 20×15 px at (30,15)"),
            "{}",
            findings[0].explanation
        );
    }

    #[test]
    fn content_larger_than_its_container_or_the_window_is_clipped() {
        let root = node(0, None, NodeKind::Container, rect(0.0, 0.0, 400.0, 300.0));
        let card = node(
            1,
            Some(0),
            NodeKind::Container,
            rect(10.0, 10.0, 100.0, 40.0),
        );
        let wide = node(2, Some(1), NodeKind::Text, rect(10.0, 10.0, 140.0, 20.0));
        // A scrollable's content is meant to be larger than it.
        let list = node(
            3,
            Some(0),
            NodeKind::Scrollable,
            rect(10.0, 60.0, 100.0, 100.0),
        );
        let mut content = node(
            4,
            Some(3),
            NodeKind::Container,
            rect(10.0, 60.0, 100.0, 900.0),
        );
        // The bridge clips what is inside a scrollable to its viewport.
        content.visible = Some(rect(10.0, 60.0, 100.0, 100.0));
        // Past the window's right edge, and its child with it (reported once).
        let mut off = node(
            5,
            None,
            NodeKind::Container,
            rect(350.0, 200.0, 100.0, 20.0),
        );
        off.layer = Layer::Overlay;
        let mut inside_off = node(6, Some(5), NodeKind::Text, rect(360.0, 200.0, 80.0, 20.0));
        inside_off.layer = Layer::Overlay;
        // Scrolled out of view: hidden, not clipped.
        let mut hidden = node(7, Some(4), NodeKind::Text, rect(10.0, 800.0, 400.0, 20.0));
        hidden.visible = None;
        let findings = diagnose(
            &snapshot(vec![
                root, card, wide, list, content, off, inside_off, hidden,
            ]),
            &[],
        );
        assert_eq!(
            kinds(&findings),
            [
                (DiagnosticKind::Clipped, vec![2, 1]),
                (DiagnosticKind::Clipped, vec![5]),
            ]
        );
        assert!(findings[0].explanation.contains("40 px past the right"));
        assert!(findings[1].explanation.contains("50 px past the right"));
    }

    #[test]
    fn controls_squeezed_to_nothing_are_clipped() {
        let mut input = node(0, None, NodeKind::TextInput, rect(10.0, 180.0, 280.0, 0.0));
        input.visible = None;
        input.text = Some("second".into());
        let mut scrolled_out = node(1, None, NodeKind::TextInput, rect(10.0, 900.0, 280.0, 30.0));
        scrolled_out.visible = None;
        let mut spacer = node(2, None, NodeKind::Container, rect(0.0, 0.0, 0.0, 0.0));
        spacer.visible = None;
        let findings = diagnose(&snapshot(vec![input, scrolled_out, spacer]), &[]);
        assert_eq!(kinds(&findings), [(DiagnosticKind::Clipped, vec![0])]);
        assert!(findings[0].explanation.contains("squeezed to 280×0"));
    }

    #[test]
    fn text_that_needs_more_room_overflows() {
        let mut long = node(0, None, NodeKind::Text, rect(0.0, 0.0, 80.0, 20.0));
        long.text = Some("A very long label".into());
        long.text_fit = Some(TextFit {
            needed: Size {
                width: 130.0,
                height: 20.0,
            },
            available_width: Some(80.0),
            available_height: None,
        });
        let mut fine = node(1, None, NodeKind::Text, rect(0.0, 30.0, 80.0, 20.0));
        fine.text_fit = Some(TextFit {
            needed: Size {
                width: 60.0,
                height: 40.0,
            },
            available_width: Some(80.0),
            available_height: None,
        });
        let findings = diagnose(&snapshot(vec![long, fine]), &[DiagnosticKind::TextOverflow]);
        assert_eq!(kinds(&findings), [(DiagnosticKind::TextOverflow, vec![0])]);
        assert!(findings[0]
            .explanation
            .contains("needs 130 px of width but has 80"));
    }

    #[test]
    fn an_id_on_two_widgets_is_a_duplicate_but_one_widget_reporting_twice_is_not() {
        let mut a = node(0, None, NodeKind::TextInput, rect(0.0, 0.0, 80.0, 20.0));
        a.id = Some("name".into());
        let mut again = node(1, None, NodeKind::Focusable, rect(0.0, 0.0, 80.0, 20.0));
        again.id = Some("name".into());
        let mut b = node(2, None, NodeKind::TextInput, rect(0.0, 40.0, 80.0, 20.0));
        b.id = Some("name".into());
        b.visible = None;
        let findings = diagnose(&snapshot(vec![a, again, b]), &[]);
        assert_eq!(
            kinds(&findings),
            [(DiagnosticKind::DuplicateId, vec![0, 2])]
        );
        assert_eq!(
            findings[0].ids,
            [Some("name".to_owned()), Some("name".to_owned())]
        );
    }

    #[test]
    fn kinds_filter_and_render() {
        let mut a = node(0, None, NodeKind::Text, rect(0.0, 0.0, 80.0, 20.0));
        a.id = Some("x".into());
        let mut b = node(1, None, NodeKind::Text, rect(0.0, 40.0, 80.0, 20.0));
        b.id = Some("x".into());
        let snap = snapshot(vec![a, b]);
        assert!(diagnose(&snap, &[DiagnosticKind::Overlap]).is_empty());
        let text = render("1", &diagnose(&snap, &[]), 2);
        assert!(text
            .starts_with("1 layout finding(s) in window 1:\n- duplicate_id: #x is on 2 widgets"));
        assert!(render("1", &[], 2).starts_with("No layout findings"));
    }
}
