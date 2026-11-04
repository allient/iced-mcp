//! Builds an AccessKit-shaped tree for MCP clients from an iced snapshot.
//!
//! This uses AccessKit's core node model and standard roles/actions only. It
//! does not attach a platform adapter or expose an operating-system tree.

use accesskit::{Action, Node as AccessNode, NodeId, Rect as AccessRect, Role, Toggled};
use iced_mcp_protocol::{Node, NodeKind, Snapshot};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

const WINDOW_NODE: u64 = 1;
const FIRST_WIDGET_NODE: u64 = 2;

/// A short token binding numeric node ids to the snapshot that produced them.
pub(crate) fn snapshot_id(snapshot: &Snapshot) -> String {
    let bytes = serde_json::to_vec(snapshot).unwrap_or_default();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Returns an AccessKit core tree projection with MCP targeting hints.
pub(crate) fn projection(snapshot: &Snapshot) -> Value {
    let all_nodes: HashMap<usize, &Node> = snapshot
        .nodes
        .iter()
        .map(|node| (node.index, node))
        .collect();
    let visible: HashMap<usize, &Node> = snapshot
        .nodes
        .iter()
        .filter(|node| node.visible.is_some())
        .map(|node| (node.index, node))
        .collect();

    let visible_parent: HashMap<usize, Option<usize>> = visible
        .values()
        .map(|node| (node.index, parent_in_view(node, &visible, &all_nodes)))
        .collect();

    let mut children: HashMap<Option<usize>, Vec<NodeId>> = HashMap::new();
    for node in visible.values() {
        let parent = visible_parent.get(&node.index).copied().flatten();
        children
            .entry(parent)
            .or_default()
            .push(node_id(node.index));
    }
    for child_nodes in children.values_mut() {
        child_nodes.sort_by_key(|id| id.0);
    }

    let mut root = AccessNode::new(Role::Window);
    root.set_bounds(AccessRect::new(
        0.0,
        0.0,
        f64::from(snapshot.size.width),
        f64::from(snapshot.size.height),
    ));
    root.set_children(children.remove(&None).unwrap_or_default());

    let mut focused = NodeId(WINDOW_NODE);
    let mut nodes = Vec::with_capacity(visible.len());
    let mut ordered: Vec<_> = visible.values().copied().collect();
    ordered.sort_by_key(|node| node.index);

    for node in ordered {
        let (role, domain_role) = accesskit_role(node);
        let mut accessible = AccessNode::new(role);
        if let Some(bounds) = node.visible {
            accessible.set_bounds(AccessRect::new(
                f64::from(bounds.x),
                f64::from(bounds.y),
                f64::from(bounds.x + bounds.width),
                f64::from(bounds.y + bounds.height),
            ));
        }
        if let Some(name) = node.label.as_deref() {
            accessible.set_label(name.to_owned());
        } else if let Some(text) = node.text.as_deref() {
            if node.kind == NodeKind::TextInput || role == Role::TextRun {
                accessible.set_value(text.to_owned());
            } else {
                accessible.set_label(text.to_owned());
            }
        }
        if let Some(value) = node.value.as_deref() {
            accessible.set_value(value.to_owned());
        }
        if let Some(value) = node.numeric_value {
            accessible.set_numeric_value(value);
        }
        if let Some(minimum) = node.min_numeric_value {
            accessible.set_min_numeric_value(minimum);
        }
        if let Some(maximum) = node.max_numeric_value {
            accessible.set_max_numeric_value(maximum);
        }
        if let Some(step) = node.numeric_value_step {
            accessible.set_numeric_value_step(step);
        }
        if let Some(description) = node.description.as_deref() {
            accessible.set_description(description.to_owned());
        }
        if let Some(placeholder) = node.placeholder.as_deref() {
            accessible.set_placeholder(placeholder.to_owned());
        }
        if node.disabled == Some(true) {
            accessible.set_disabled();
        }
        if let Some(checked) = node.checked {
            if matches!(
                role,
                Role::MenuItemRadio | Role::RadioButton | Role::Tab | Role::ListItem
            ) {
                accessible.set_selected(checked);
            } else {
                accessible.set_toggled(Toggled::from(checked));
            }
        }
        if node.indeterminate == Some(true) {
            accessible.set_toggled(Toggled::Mixed);
        }
        if let Some(selected) = node.selected {
            accessible.set_selected(selected);
        }
        if let Some(expanded) = node.expanded {
            accessible.set_expanded(expanded);
        }
        if let Some(id) = node.id.as_deref() {
            accessible.set_author_id(id.to_owned());
        }
        if let Some(domain_role) = domain_role {
            accessible.set_role_description(domain_role.to_owned());
        }
        accessible.set_children(children.remove(&Some(node.index)).unwrap_or_default());

        let mut actions = Vec::new();
        if node.disabled != Some(true)
            && (node.kind == NodeKind::TextInput
                || node.focused.is_some()
                || matches!(
                    role,
                    Role::CheckBox
                        | Role::MenuItem
                        | Role::MenuItemCheckBox
                        | Role::MenuItemRadio
                        | Role::RadioButton
                        | Role::Slider
                        | Role::SpinButton
                        | Role::Switch
                        | Role::Tab
                ))
        {
            accessible.add_action(Action::Focus);
            actions.push("focus");
        }
        if node.disabled != Some(true)
            && matches!(role, Role::Slider | Role::SpinButton)
            && node.numeric_value.is_some()
            && node.numeric_value_step.is_some()
        {
            accessible.add_action(Action::Increment);
            accessible.add_action(Action::Decrement);
            actions.extend(["increment", "decrement"]);
            if role == Role::Slider || role == Role::SpinButton {
                accessible.add_action(Action::SetValue);
                actions.push("set_value");
            }
        }
        if node.disabled != Some(true)
            && node.kind == NodeKind::Semantic
            && matches!(node.role.as_deref(), Some("switch" | "checkbox"))
            && node.checked.is_some()
        {
            actions.push("set_value");
        }
        if node.disabled != Some(true)
            && node.kind == NodeKind::Semantic
            && node.role.as_deref() == Some("combobox")
            && node
                .choices
                .as_ref()
                .is_some_and(|choices| !choices.is_empty())
        {
            accessible.add_action(Action::Click);
            accessible.add_action(Action::SetValue);
            actions.extend(["click", "set_value"]);
        }
        if node.disabled != Some(true)
            && node.kind == NodeKind::Semantic
            && node.role.as_deref() == Some("time")
        {
            accessible.add_action(Action::Click);
            accessible.add_action(Action::SetValue);
            actions.extend(["click", "set_value"]);
        }
        if node.disabled != Some(true)
            && node.kind == NodeKind::Semantic
            && node.role.as_deref() == Some("textbox")
        {
            accessible.add_action(Action::Click);
            accessible.add_action(Action::Focus);
            accessible.add_action(Action::SetValue);
            actions.extend(["click", "focus", "set_value"]);
        }
        if node.disabled != Some(true)
            && node.kind == NodeKind::Semantic
            && node.role.as_deref() == Some("date")
        {
            accessible.add_action(Action::Click);
            actions.push("click");
            if node.description.as_deref().is_some_and(|description| {
                description.starts_with("Single date picker")
                    || description.starts_with("Date range picker")
            }) {
                accessible.add_action(Action::SetValue);
                actions.push("set_value");
            }
        }
        if node.disabled != Some(true) && node.kind == NodeKind::TextInput {
            accessible.add_action(Action::ReplaceSelectedText);
            actions.push("replace_selected_text");
        }
        if node.disabled != Some(true)
            && (node.interaction.as_deref() == Some("pointer")
                || matches!(
                    role,
                    Role::Button
                        | Role::CheckBox
                        | Role::MenuItem
                        | Role::MenuItemCheckBox
                        | Role::MenuItemRadio
                        | Role::RadioButton
                        | Role::Switch
                        | Role::Tab
                ))
        {
            accessible.add_action(Action::Click);
            actions.push("click");
        }
        if node.disabled != Some(true) {
            if node.expanded == Some(false) {
                accessible.add_action(Action::Expand);
                actions.push("expand");
            } else if node.expanded == Some(true) {
                accessible.add_action(Action::Collapse);
                actions.push("collapse");
            }
        }
        if node.kind == NodeKind::Scrollable {
            accessible.add_action(Action::ScrollUp);
            accessible.add_action(Action::ScrollDown);
            actions.extend(["scroll_up", "scroll_down"]);
            if let Some(scroll) = node.scroll {
                accessible.set_scroll_y(f64::from(scroll.offset_y));
                accessible.set_scroll_y_min(0.0);
                accessible.set_scroll_y_max(f64::from(
                    (scroll.content_height - node.bounds.height).max(0.0),
                ));
            }
        }
        if node.focused == Some(true) {
            focused = node_id(node.index);
        }

        let id = node_id(node.index).0;
        let target = node.id.as_ref().map_or_else(
            || {
                node.label.as_ref().or(node.text.as_ref()).map_or_else(
                    || {
                        let bounds = node.visible.unwrap_or(node.bounds);
                        json!({
                            "x": bounds.x + bounds.width / 2.0,
                            "y": bounds.y + bounds.height / 2.0,
                        })
                    },
                    |text| json!({ "text": text }),
                )
            },
            |id| json!({ "id": id }),
        );
        nodes.push(json!({
            "id": id,
            "parent": visible_parent.get(&node.index).copied().flatten().map(|index| node_id(index).0).unwrap_or(WINDOW_NODE),
            "mcpTarget": target,
            "layer": node.layer,
            "interaction": node.interaction,
            "domainRole": domain_role,
            "supportedActions": actions,
            "numericControl": node.numeric_value.map(|value| json!({
                "value": value,
                "min": node.min_numeric_value,
                "max": node.max_numeric_value,
                "step": node.numeric_value_step,
                "trackBounds": node.track_bounds,
                "vertical": node.vertical,
                "inverted": node.inverted,
            })),
            "choices": node.choices,
            "node": serde_json::to_value(accessible).unwrap_or(Value::Null),
        }));
    }

    json!({
        "rootId": WINDOW_NODE,
        "focusedId": focused.0,
        "root": serde_json::to_value(root).unwrap_or(Value::Null),
        "nodes": nodes,
    })
}

fn node_id(index: usize) -> NodeId {
    NodeId(FIRST_WIDGET_NODE + index as u64)
}

fn parent_in_view(
    node: &Node,
    visible: &HashMap<usize, &Node>,
    all_nodes: &HashMap<usize, &Node>,
) -> Option<usize> {
    let mut parent = node.parent;
    while let Some(index) = parent {
        if visible.contains_key(&index) {
            return Some(index);
        }
        parent = all_nodes.get(&index).and_then(|ancestor| ancestor.parent);
    }
    None
}

fn accesskit_role(node: &Node) -> (Role, Option<&str>) {
    if let Some(role) = node.role.as_deref() {
        return semantic_role(role);
    }

    match node.kind {
        NodeKind::Scrollable => (Role::ScrollView, None),
        NodeKind::Focusable if node.interaction.as_deref() == Some("pointer") => {
            (Role::Button, None)
        }
        NodeKind::Focusable => (Role::GenericContainer, None),
        NodeKind::TextInput => (Role::TextInput, None),
        NodeKind::Text => (Role::TextRun, None),
        NodeKind::Custom => (Role::GraphicsObject, Some("custom graphics")),
        NodeKind::Region if node.interaction.as_deref() == Some("pointer") => (Role::Button, None),
        NodeKind::Region => (Role::GraphicsObject, Some("interactive region")),
        NodeKind::Label => (Role::Label, None),
        NodeKind::Container => (Role::GenericContainer, None),
        NodeKind::Semantic => (Role::Unknown, None),
    }
}

fn semantic_role(role: &str) -> (Role, Option<&str>) {
    match role {
        "button" => (Role::Button, None),
        "checkbox" => (Role::CheckBox, None),
        "combobox" => (Role::ComboBox, None),
        "time" => (Role::TimeInput, None),
        "date" => (Role::DateInput, None),
        "calendar" => (Role::GraphicsDocument, Some("calendar")),
        "radio" | "radiobutton" => (Role::RadioButton, None),
        "switch" => (Role::Switch, None),
        "slider" => (Role::Slider, None),
        "spinbutton" => (Role::SpinButton, None),
        "textbox" | "textinput" => (Role::TextInput, None),
        "text" | "label" => (Role::Label, None),
        "img" | "image" => (Role::Image, None),
        "group" => (Role::Group, None),
        "cell" => (Role::Cell, None),
        "row" => (Role::Row, None),
        "menuitem" => (Role::MenuItem, None),
        "menuitemcheckbox" => (Role::MenuItemCheckBox, None),
        "menuitemradio" => (Role::MenuItemRadio, None),
        "list" => (Role::List, None),
        "listitem" => (Role::ListItem, None),
        "tooltip" => (Role::Tooltip, None),
        "tab" => (Role::Tab, None),
        "tablist" => (Role::TabList, None),
        "tabpanel" => (Role::TabPanel, None),
        "radiogroup" => (Role::RadioGroup, None),
        "grid" => (Role::Grid, None),
        "chart" | "heatmap" | "plot3d" | "profile" => (Role::GraphicsDocument, Some(role)),
        "bar" | "point" | "line" | "plane" | "axis" | "tick" | "drawing" | "handle"
        | "crosshair" | "series" | "trade" | "wall" => (Role::GraphicsObject, Some(role)),
        "scroll_area" | "scrollview" => (Role::ScrollView, None),
        "interactive_region" => (Role::GraphicsObject, Some(role)),
        _ => (Role::Unknown, Some(role)),
    }
}
