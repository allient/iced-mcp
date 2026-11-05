//! Turns a raw snapshot into the compact text an agent reads, and finds the
//! node a target names.
//!
//! A raw tree is mostly layout: rows, columns and padding containers around
//! the few widgets that matter. The outline keeps text, inputs, focusables,
//! scrollables, anything with an id and any leaf the app shows a pointer
//! over, and indents each by its nearest kept ancestor.

use iced_mcp_protocol::{Layer, Node, NodeKind, Rect, Snapshot};

use std::fmt::Write as _;

/// Semantic nodes of one role shown per parent before the rest are counted.
const SEMANTIC_PER_ROLE: usize = 20;

pub(crate) struct Outline {
    pub(crate) text: String,
    pub(crate) shown: usize,
}

pub(crate) struct OutlineOptions<'a> {
    pub(crate) query: Option<&'a str>,
    pub(crate) include_hidden: bool,
    pub(crate) max_lines: usize,
}

fn kept(node: &Node, has_child: bool, include_hidden: bool, labels: &[Rect]) -> bool {
    if node.visible.is_none() && !include_hidden {
        return false;
    }
    match node.kind {
        NodeKind::Container => {
            // An icon button's box is shown by its label instead.
            let labelled = || {
                let (x, y) = node.visible.unwrap_or(node.bounds).center();
                labels.iter().any(|r| r.contains(x, y))
            };
            node.id.is_some() || (!has_child && node.interaction.is_some() && !labelled())
        }
        NodeKind::Custom => node.id.is_some(),
        NodeKind::Region | NodeKind::Semantic => true,
        _ => true,
    }
}

pub(crate) fn outline(snapshot: &Snapshot, options: &OutlineOptions<'_>) -> Outline {
    let nodes = &snapshot.nodes;
    let mut has_child = vec![false; nodes.len()];
    for node in nodes {
        if let Some(slot) = node.parent.and_then(|p| has_child.get_mut(p)) {
            *slot = true;
        }
    }
    let labels: Vec<Rect> = nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Label)
        .filter_map(|n| n.visible)
        .collect();
    let keep: Vec<bool> = nodes
        .iter()
        .zip(&has_child)
        .map(|(node, has_child)| kept(node, *has_child, options.include_hidden, &labels))
        .collect();

    let query = options.query.map(str::to_lowercase);
    let matches = |node: &Node| match &query {
        None => true,
        Some(q) => {
            node.text
                .as_deref()
                .is_some_and(|t| t.to_lowercase().contains(q))
                || node
                    .id
                    .as_deref()
                    .is_some_and(|i| i.to_lowercase().contains(q))
        }
    };

    let mut text = String::new();
    let mut shown = 0;
    let mut truncated = 0;
    let mut last_shown: Option<usize> = None;
    let mut per_role: std::collections::HashMap<(Option<usize>, Option<String>), usize> =
        std::collections::HashMap::new();
    // Depth among kept nodes: a node sits one deeper than its nearest kept ancestor.
    let mut depth = vec![0_usize; nodes.len()];
    for (i, node) in nodes.iter().enumerate() {
        let ancestor_depth =
            std::iter::successors(node.parent, |&p| nodes.get(p).and_then(|n| n.parent))
                .find(|&p| keep.get(p).copied().unwrap_or(false))
                .map(|p| depth[p] + 1)
                .unwrap_or(0);
        depth[i] = ancestor_depth;
        if !keep[i] || !matches(node) {
            continue;
        }
        // The same text drawn twice in one place (a measuring pass, a
        // cross-fade) is one thing on screen.
        let duplicate = last_shown.is_some_and(|j: usize| {
            nodes.get(j).is_some_and(|prev| {
                prev.kind == node.kind && prev.text == node.text && prev.bounds == node.bounds
            })
        });
        if duplicate {
            continue;
        }
        // A chart can describe hundreds of bars: show the first few of each
        // role per parent and count the rest.
        if node.kind == NodeKind::Semantic && query.is_none() {
            let key = (node.parent, node.role.clone());
            let seen = per_role.entry(key).or_insert(0);
            *seen += 1;
            if *seen > SEMANTIC_PER_ROLE {
                continue;
            }
        }
        last_shown = Some(i);
        if shown >= options.max_lines {
            truncated += 1;
            continue;
        }
        shown += 1;
        let indent = if query.is_some() { 0 } else { depth[i] };
        let _ = writeln!(text, "{}{}", "  ".repeat(indent), line(node));
    }
    let mut capped: Vec<String> = per_role
        .iter()
        .filter(|(_, n)| **n > SEMANTIC_PER_ROLE)
        .map(|((parent, role), n)| {
            format!(
                "… {} more {} under [{}] — `snapshot` with `query` lists them all",
                n - SEMANTIC_PER_ROLE,
                role.as_deref().unwrap_or("node"),
                parent.map_or_else(|| "?".to_owned(), |p| p.to_string())
            )
        })
        .collect();
    capped.sort();
    for line in capped {
        let _ = writeln!(text, "{line}");
    }
    if truncated > 0 {
        let _ = writeln!(
            text,
            "… {truncated} more not shown: narrow with `query`, or raise `max_lines`"
        );
    }
    Outline { text, shown }
}

/// The nodes `snapshot` lists one level under `index` (hidden ones
/// included): its descendants whose nearest listed ancestor it is. Layout
/// boxes in between do not count, so a list's items are its children even
/// when a column wraps them.
pub(crate) fn outline_children(snapshot: &Snapshot, index: usize) -> Vec<&Node> {
    let nodes = &snapshot.nodes;
    let mut has_child = vec![false; nodes.len()];
    for node in nodes {
        if let Some(slot) = node.parent.and_then(|p| has_child.get_mut(p)) {
            *slot = true;
        }
    }
    let labels: Vec<Rect> = nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Label)
        .filter_map(|n| n.visible)
        .collect();
    let keep: Vec<bool> = nodes
        .iter()
        .zip(&has_child)
        .map(|(node, has_child)| kept(node, *has_child, true, &labels))
        .collect();
    nodes
        .iter()
        .filter(|node| node.index != index && keep[node.index])
        .filter(|node| {
            std::iter::successors(node.parent, |&p| nodes.get(p).and_then(|n| n.parent))
                .find(|&p| keep.get(p).copied().unwrap_or(false) || p == index)
                == Some(index)
        })
        .collect()
}

/// `[12] text "Save" #save-button (100,40 80×24) pointer`
pub(crate) fn line(node: &Node) -> String {
    let kind = match (&node.kind, &node.role) {
        (NodeKind::Semantic, Some(role)) => role.as_str(),
        (kind, _) => kind_name(*kind),
    };
    let mut out = format!("[{}] {kind}", node.index);
    if let Some(text) = &node.text {
        let _ = write!(out, " {text:?}");
    }
    if let Some(value) = &node.value {
        let _ = write!(out, " = {value}");
    } else if let Some(value) = node.numeric_value {
        let _ = write!(out, " = {value}");
    }
    if let (Some(minimum), Some(maximum)) = (node.min_numeric_value, node.max_numeric_value) {
        let _ = write!(out, " [{minimum}…{maximum}");
        if let Some(step) = node.numeric_value_step {
            let _ = write!(out, " step {step}");
        }
        out.push(']');
    }
    if let Some(id) = &node.id {
        let _ = write!(out, " #{id}");
    }
    let r = node.visible.unwrap_or(node.bounds);
    let _ = write!(
        out,
        " ({:.0},{:.0} {:.0}×{:.0})",
        r.x, r.y, r.width, r.height
    );
    if node.visible.is_none() {
        out.push_str(" hidden");
    }
    if let Some(interaction) = &node.interaction {
        let _ = write!(out, " {interaction}");
    }
    if node.focused == Some(true) {
        out.push_str(" focused");
    }
    if let Some(scroll) = &node.scroll {
        let _ = write!(
            out,
            " scroll({:.0},{:.0} of {:.0}×{:.0})",
            scroll.offset_x, scroll.offset_y, scroll.content_width, scroll.content_height
        );
    }
    if node.layer == Layer::Overlay {
        out.push_str(" overlay");
    }
    out
}

fn kind_name(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Container => "box",
        NodeKind::Scrollable => "scroll",
        NodeKind::Focusable => "focusable",
        NodeKind::TextInput => "input",
        NodeKind::Text => "text",
        NodeKind::Custom => "custom",
        NodeKind::Region => "region",
        NodeKind::Label => "label",
        NodeKind::Semantic => "semantic",
    }
}

/// What a target selects, before it is resolved to a point.
pub(crate) enum Selector<'a> {
    Text { text: &'a str, exact: bool },
    Id(&'a str),
}

/// The visible nodes `selector` matches, best first: overlay before base,
/// interactive before inert, then the smallest (most specific).
pub(crate) fn find<'a>(snapshot: &'a Snapshot, selector: &Selector<'_>) -> Vec<&'a Node> {
    let mut found: Vec<&Node> = snapshot
        .nodes
        .iter()
        .filter(|node| node.visible.is_some())
        .filter(|node| selects(node, selector))
        .collect();
    found.sort_by(|a, b| {
        let layer = |n: &Node| u8::from(n.layer == Layer::Base);
        // What reacts to a click beats what is only drawn: a label shown
        // both in a list and on a chart means the list item.
        let inert = |n: &Node| u8::from(n.interaction.is_none());
        layer(a)
            .cmp(&layer(b))
            .then(inert(a).cmp(&inert(b)))
            .then(area(a).total_cmp(&area(b)))
            .then(a.index.cmp(&b.index))
    });
    found
}

/// Whether `selector` names `node`, visible or not.
pub(crate) fn selects(node: &Node, selector: &Selector<'_>) -> bool {
    match selector {
        Selector::Id(id) => node.id.as_deref() == Some(*id),
        Selector::Text { text, exact: true } => node.text.as_deref() == Some(*text),
        Selector::Text { text, exact: false } => node
            .text
            .as_deref()
            .is_some_and(|t| t.to_lowercase().contains(&text.to_lowercase())),
    }
}

fn area(node: &Node) -> f32 {
    node.visible.map_or(f32::MAX, |r| r.area())
}

/// Texts on screen, for "not found — did you mean" errors.
pub(crate) fn visible_texts(snapshot: &Snapshot, limit: usize) -> Vec<String> {
    let mut texts: Vec<String> = Vec::new();
    for node in &snapshot.nodes {
        if node.visible.is_none() {
            continue;
        }
        if let Some(text) = node
            .text
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            if !texts.iter().any(|t| t == text) {
                texts.push(text.to_owned());
            }
        }
        if texts.len() >= limit {
            break;
        }
    }
    texts
}

/// What "the screen changed" means for settling: every visible node's kind,
/// text and box, with digits dropped from the text and the box snapped to
/// 4 px. Live numbers (a ticking price, a clock) and sub-pixel motion do not
/// count, so an app streaming data can still settle; Argent rounds its
/// frames for the same reason.
pub(crate) fn signature(snapshot: &Snapshot) -> String {
    let mut out = String::new();
    for node in &snapshot.nodes {
        // Drawn content (live bars) changes all the time: not "the screen".
        if node.kind == NodeKind::Semantic {
            continue;
        }
        let Some(r) = node.visible else { continue };
        let text: String = node
            .text
            .as_deref()
            .unwrap_or("")
            .chars()
            .filter(|c| !c.is_ascii_digit())
            .collect();
        let snap = |v: f32| (v / 4.0).round() as i32;
        let _ = writeln!(
            out,
            "{:?}|{text}|{},{},{},{}",
            node.kind,
            snap(r.x),
            snap(r.y),
            snap(r.width),
            snap(r.height)
        );
    }
    out
}

/// The visible nodes under a point, innermost (smallest) first.
pub(crate) fn at_point(snapshot: &Snapshot, x: f32, y: f32) -> Vec<&Node> {
    let mut hits: Vec<&Node> = snapshot
        .nodes
        .iter()
        .filter(|n| n.visible.is_some_and(|r| r.contains(x, y)))
        .collect();
    hits.sort_by(|a, b| {
        let layer = |n: &Node| u8::from(n.layer == Layer::Base);
        layer(a).cmp(&layer(b)).then(area(a).total_cmp(&area(b)))
    });
    hits
}

/// Semantic gaps that an app author can act on. Machine ids are targets, not
/// human-readable names; generic hit-test regions are called out separately.
pub(crate) fn audit_findings(snapshot: &Snapshot) -> Vec<(&Node, &'static str)> {
    let mut has_child = vec![false; snapshot.nodes.len()];
    for node in &snapshot.nodes {
        if let Some(slot) = node.parent.and_then(|p| has_child.get_mut(p)) {
            *slot = true;
        }
    }
    let labels: Vec<Rect> = snapshot
        .nodes
        .iter()
        .filter(|n| {
            matches!(n.kind, NodeKind::Label | NodeKind::Text)
                || (n.kind == NodeKind::Semantic
                    && matches!(
                        n.role.as_deref(),
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
        })
        .filter_map(|n| n.visible)
        .collect();
    snapshot
        .nodes
        .iter()
        .zip(has_child)
        .filter_map(|(node, has_child)| {
            let visible = node.visible?;
            let clickable = node.interaction.as_deref() == Some("pointer");
            let (cx, cy) = visible.center();
            let named = node
                .label
                .as_deref()
                .or(node.text.as_deref())
                .is_some_and(|text| !text.trim().is_empty())
                || labels.iter().any(|r| r.contains(cx, cy));
            let issue = match node.kind {
                NodeKind::Region if node.interaction.as_deref() == Some("grab") => None,
                NodeKind::Region if !named => Some(
                    "approximate hit-test region; the owning widget should report semantic nodes",
                ),
                NodeKind::Container if clickable && !has_child && !named => {
                    Some("clickable widget has no human-readable name")
                }
                NodeKind::Focusable | NodeKind::TextInput if !named => {
                    Some("interactive widget has no human-readable name")
                }
                NodeKind::Semantic if node.role.is_none() => Some("semantic node has no role"),
                NodeKind::Semantic
                    if matches!(node.role.as_deref(), Some("checkbox" | "switch"))
                        && node.checked.is_none()
                        && node.indeterminate.is_none() =>
                {
                    Some("toggle control does not report checked state")
                }
                NodeKind::Semantic
                    if matches!(node.role.as_deref(), Some("radio" | "tab" | "listitem"))
                        && node.selected.is_none()
                        && node.checked.is_none() =>
                {
                    Some("selectable control does not report selected state")
                }
                NodeKind::Semantic
                    if matches!(node.role.as_deref(), Some("slider" | "spinbutton"))
                        && node.numeric_value.is_none() =>
                {
                    Some("numeric control does not report its current value")
                }
                NodeKind::Semantic
                    if matches!(node.role.as_deref(), Some("slider" | "spinbutton"))
                        && node.numeric_value_step.is_none() =>
                {
                    Some("numeric control does not report its step size")
                }
                NodeKind::Semantic
                    if node.role.as_deref() == Some("slider")
                        && (node.min_numeric_value.is_none()
                            || node.max_numeric_value.is_none()
                            || node.track_bounds.is_none()) =>
                {
                    Some("slider does not report its range and track bounds")
                }
                NodeKind::Semantic
                    if node.role.as_deref() == Some("combobox")
                        && node.choices.as_ref().is_none_or(Vec::is_empty) =>
                {
                    Some("select does not report its enabled options")
                }
                NodeKind::Semantic
                    if node.role.as_deref() == Some("calendar") && node.value.is_none() =>
                {
                    Some("calendar does not report its visible month")
                }
                NodeKind::Custom if !has_child && clickable => {
                    Some("custom interactive widget reports no semantic children")
                }
                _ => None,
            };
            issue.map(|issue| (node, issue))
        })
        .collect()
}

/// The point to aim at inside a node: its visible centre.
pub(crate) fn aim(node: &Node) -> (f32, f32) {
    node.visible.unwrap_or(node.bounds).center()
}

pub(crate) fn rect_text(r: &Rect) -> String {
    format!("({:.0},{:.0} {:.0}×{:.0})", r.x, r.y, r.width, r.height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_mcp_protocol::Size;

    fn node(index: usize, parent: Option<usize>, kind: NodeKind, text: Option<&str>) -> Node {
        let r = Rect {
            x: 0.0,
            y: index as f32 * 10.0,
            width: 100.0 - index as f32,
            height: 10.0,
        };
        Node {
            index,
            parent,
            kind,
            layer: Layer::Base,
            id: None,
            text: text.map(str::to_owned),
            bounds: r,
            visible: Some(r),
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
        }
    }

    fn snapshot(nodes: Vec<Node>) -> Snapshot {
        Snapshot {
            window: "1".into(),
            size: Size {
                width: 800.0,
                height: 600.0,
            },
            nodes,
        }
    }

    #[test]
    fn layout_boxes_collapse_and_text_is_indented_under_kept_ancestors() {
        let mut list = node(1, Some(0), NodeKind::Scrollable, None);
        list.id = Some("list".into());
        let snap = snapshot(vec![
            node(0, None, NodeKind::Container, None),
            list,
            node(2, Some(1), NodeKind::Container, None),
            node(3, Some(2), NodeKind::Text, Some("Row A")),
        ]);
        let out = outline(
            &snap,
            &OutlineOptions {
                query: None,
                include_hidden: false,
                max_lines: 100,
            },
        );
        let lines: Vec<&str> = out.text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("[1] scroll #list"));
        assert!(lines[1].starts_with("  [3] text \"Row A\""));
    }

    #[test]
    fn a_label_replaces_its_icon_buttons_bare_box() {
        let mut button = node(0, None, NodeKind::Container, None);
        button.interaction = Some("pointer".into());
        let mut label = node(1, None, NodeKind::Label, Some("Close"));
        label.bounds = button.bounds;
        label.visible = button.visible;
        let snap = snapshot(vec![button, label]);
        let out = outline(
            &snap,
            &OutlineOptions {
                query: None,
                include_hidden: false,
                max_lines: 100,
            },
        );
        assert_eq!(out.shown, 1);
        assert!(out.text.starts_with("[1] label \"Close\""));
        let found = find(
            &snap,
            &Selector::Text {
                text: "close",
                exact: false,
            },
        );
        assert_eq!(found[0].index, 1);
    }

    #[test]
    fn signatures_ignore_ticking_numbers_and_jitter() {
        let a = snapshot(vec![node(0, None, NodeKind::Text, Some("71890.9"))]);
        let mut moved = node(0, None, NodeKind::Text, Some("71891.2"));
        moved.visible = moved.visible.map(|mut r| {
            r.x += 0.6;
            r
        });
        let b = snapshot(vec![moved]);
        assert_eq!(signature(&a), signature(&b));
        let c = snapshot(vec![node(0, None, NodeKind::Text, Some("Saved"))]);
        assert_ne!(signature(&a), signature(&c));
    }

    #[test]
    fn audit_flags_missing_human_names_but_accepts_named_ids() {
        let mut bare = node(0, None, NodeKind::Container, None);
        bare.interaction = Some("pointer".into());
        let mut named = node(1, None, NodeKind::Container, None);
        named.interaction = Some("pointer".into());
        named.id = Some("save".into());
        named.label = Some("Save".into());
        let snap = snapshot(vec![bare, named]);
        let flagged: Vec<usize> = audit_findings(&snap)
            .iter()
            .map(|(node, _)| node.index)
            .collect();
        assert_eq!(flagged, [0]);
    }

    #[test]
    fn semantic_nodes_show_their_role_and_are_capped() {
        let mut chart = node(0, None, NodeKind::Custom, None);
        chart.id = Some("chart".into());
        let mut nodes = vec![chart];
        for i in 1..=25 {
            let mut bar = node(i, Some(0), NodeKind::Semantic, Some(&format!("12:{i:02}")));
            bar.role = Some("bar".into());
            bar.value = Some("O 1 C 2".into());
            nodes.push(bar);
        }
        let snap = snapshot(nodes);
        let out = outline(
            &snap,
            &OutlineOptions {
                query: None,
                include_hidden: false,
                max_lines: 300,
            },
        );
        assert!(out.text.contains("[1] bar \"12:01\" = O 1 C 2"));
        assert!(out.text.contains("… 5 more bar under [0]"));
        assert_eq!(
            signature(&snap),
            signature(&snapshot(vec![snap.nodes[0].clone()]))
        );
        let found = find(
            &snap,
            &Selector::Text {
                text: "12:25",
                exact: true,
            },
        );
        assert_eq!(found[0].index, 25);

        // The same text on a clickable list item and on a drawn bar: the item wins.
        let mut item = node(30, None, NodeKind::Text, Some("12:25"));
        item.interaction = Some("pointer".into());
        item.bounds.width = 500.0;
        item.visible = Some(item.bounds);
        let mut with_item = snap.nodes.clone();
        with_item.push(item);
        let snap = snapshot(with_item);
        let found = find(
            &snap,
            &Selector::Text {
                text: "12:25",
                exact: true,
            },
        );
        assert_eq!(found[0].index, 30);
    }

    #[test]
    fn find_prefers_overlay_then_the_smallest_match() {
        let base = node(0, None, NodeKind::Text, Some("Open"));
        let mut menu = node(1, None, NodeKind::Text, Some("Open recent"));
        menu.layer = Layer::Overlay;
        let snap = snapshot(vec![base, menu]);
        let found = find(
            &snap,
            &Selector::Text {
                text: "open",
                exact: false,
            },
        );
        assert_eq!(found[0].index, 1);
        let exact = find(
            &snap,
            &Selector::Text {
                text: "Open",
                exact: true,
            },
        );
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].index, 0);
    }
}
