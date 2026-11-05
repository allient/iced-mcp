//! Structural assertions: `expect` checks a target's presence, state, text,
//! geometry relative to other targets, text fit and children against a
//! snapshot; `expect_pixel` checks one pixel's colour. A failure says what
//! was expected and what was found, check by check.

use crate::params::{ExpectParams, Target};
use crate::view::{self, Selector};

use iced_mcp_protocol::{Layer, Node, NodeKind, Rect, Snapshot};
use std::fmt::Write as _;

/// Edges and centres within this many logical pixels count as aligned or
/// touching, unless `tolerance` says otherwise.
pub(crate) const DEFAULT_TOLERANCE: f32 = 2.0;

/// The nodes a target names, visible ones first (in `find` order), then
/// hidden ones: `exists`/`visible: false` must see what is scrolled away.
pub(crate) fn matches<'a>(
    snapshot: &'a Snapshot,
    target: &Target,
) -> Result<Vec<&'a Node>, String> {
    if let Some(reference) = target.reference {
        return Ok(snapshot.nodes.get(reference).into_iter().collect());
    }
    if let (Some(x), Some(y)) = (target.x, target.y) {
        return Ok(view::at_point(snapshot, x, y).into_iter().take(1).collect());
    }
    let selector = if let Some(id) = &target.id {
        Selector::Id(id)
    } else if let Some(text) = &target.text {
        Selector::Text {
            text,
            exact: target.exact.unwrap_or(false),
        }
    } else {
        return Err("no target: give text, id, ref, or x and y".into());
    };
    let mut found = view::find(snapshot, &selector);
    found.extend(
        snapshot
            .nodes
            .iter()
            .filter(|n| n.visible.is_none() && view::selects(n, &selector)),
    );
    Ok(found)
}

/// How a target is named in messages: `#save`, `"Add"`, `[12]`, `(10,20)`.
pub(crate) fn describe(target: &Target) -> String {
    if let Some(id) = &target.id {
        format!("#{id}")
    } else if let Some(text) = &target.text {
        format!("{text:?}")
    } else if let Some(reference) = target.reference {
        format!("[{reference}]")
    } else if let (Some(x), Some(y)) = (target.x, target.y) {
        format!("({x:.0},{y:.0})")
    } else {
        "the target".into()
    }
}

fn area(node: &Node) -> Rect {
    node.visible.unwrap_or(node.bounds)
}

/// Checks every condition in `p` against `snapshot`: `Ok` with a summary
/// when all hold, `Err` listing each one that does not.
pub(crate) fn evaluate(snapshot: &Snapshot, p: &ExpectParams) -> Result<String, String> {
    let name = describe(&p.target);
    let found = matches(snapshot, &p.target)?;
    let visible: Vec<&Node> = found
        .iter()
        .copied()
        .filter(|n| n.visible.is_some())
        .collect();
    let mut report = Report::default();
    let check = |report: &mut Report, ok: bool, expected: String, got: String| {
        if ok {
            report.passed.push(expected);
        } else {
            report
                .failures
                .push(format!("expected {expected}, found {got}"));
        }
    };

    let has_node_checks = p.visible == Some(true)
        || p.enabled.is_some()
        || p.focused.is_some()
        || p.text_equals.is_some()
        || p.text_contains.is_some()
        || p.value.is_some()
        || p.fits.is_some()
        || p.children.is_some()
        || p.relations().next().is_some();
    let exists = p.exists.unwrap_or(true);
    if p.exists.is_some() || !has_node_checks && p.visible.is_none() && p.count.is_none() {
        check(
            &mut report,
            found.is_empty() != exists,
            format!("{name} to {}", if exists { "exist" } else { "not exist" }),
            match found.first() {
                Some(node) => view::line(node),
                None => "nothing".into(),
            },
        );
    }
    if p.visible == Some(false) {
        check(
            &mut report,
            visible.is_empty(),
            format!("{name} not to be visible"),
            visible.first().map_or_else(String::new, |n| view::line(n)),
        );
    }
    if let Some(count) = p.count {
        check(
            &mut report,
            visible.len() == count,
            format!("{count} visible match(es) for {name}"),
            format!("{}", visible.len()),
        );
    }

    if has_node_checks {
        let nth = p.target.nth.unwrap_or(0);
        let Some(node) = found.get(nth).copied() else {
            let texts = view::visible_texts(snapshot, 20);
            return Err(format!(
                "expected {name} (match {}) to exist, found {} match(es). Visible text: {texts:?}",
                nth + 1,
                found.len()
            ));
        };
        let line = view::line(node);
        if p.visible == Some(true) {
            check(
                &mut report,
                node.visible.is_some(),
                format!("{name} to be visible"),
                format!("it hidden (scrolled or clipped out): {line}"),
            );
        }
        if let Some(enabled) = p.enabled {
            let (is, basis) = match node.disabled {
                Some(disabled) => (!disabled, "its reported state"),
                None => (
                    node.interaction.is_some(),
                    "its cursor: no widget state is reported, so a control showing no interactive cursor counts as disabled",
                ),
            };
            check(
                &mut report,
                is == enabled,
                format!(
                    "{name} to be {}",
                    if enabled { "enabled" } else { "disabled" }
                ),
                format!(
                    "it {} (judged by {basis}): {line}",
                    if is { "enabled" } else { "disabled" }
                ),
            );
        }
        if let Some(focused) = p.focused {
            let is = node.focused == Some(true);
            check(
                &mut report,
                is == focused,
                format!(
                    "{name} to be {}",
                    if focused { "focused" } else { "unfocused" }
                ),
                format!("it {}: {line}", if is { "focused" } else { "not focused" }),
            );
        }
        let text = node.text.as_deref().unwrap_or("");
        if let Some(expected) = &p.text_equals {
            check(
                &mut report,
                text == expected,
                format!("{name} to have text {expected:?}"),
                format!("{text:?}"),
            );
        }
        if let Some(expected) = &p.text_contains {
            check(
                &mut report,
                text.to_lowercase().contains(&expected.to_lowercase()),
                format!("{name}'s text to contain {expected:?}"),
                format!("{text:?}"),
            );
        }
        if let Some(expected) = &p.value {
            let value = node
                .value
                .clone()
                .or_else(|| node.numeric_value.map(|v| v.to_string()))
                .or_else(|| matches!(node.kind, NodeKind::TextInput).then(|| text.to_owned()));
            check(
                &mut report,
                value.as_deref() == Some(expected.as_str()),
                format!("{name} to have value {expected:?}"),
                match &value {
                    Some(value) => format!("{value:?}"),
                    None => format!("no value reported: {line}"),
                },
            );
        }
        if let Some(fits) = p.fits {
            let (ok, got) = fit(snapshot, node, fits);
            check(
                &mut report,
                ok,
                format!(
                    "{name}'s text {}",
                    if fits { "to fit" } else { "not to fit" }
                ),
                got,
            );
        }
        if let Some(children) = p.children {
            let kids = view::outline_children(snapshot, node.index);
            check(
                &mut report,
                kids.len() == children,
                format!("{name} to have {children} child(ren)"),
                format!(
                    "{}{}",
                    kids.len(),
                    if kids.is_empty() {
                        String::new()
                    } else {
                        let first: Vec<String> =
                            kids.iter().take(5).map(|k| view::line(k)).collect();
                        format!(": {}", first.join("; "))
                    }
                ),
            );
        }
        let tolerance = p.tolerance.unwrap_or(DEFAULT_TOLERANCE);
        for (relation, other) in p.relations() {
            let other_name = describe(other);
            let Some(other_node) = matches(snapshot, other)?.first().copied() else {
                report.failures.push(format!(
                    "expected {name} {relation} {other_name}, found no {other_name}"
                ));
                continue;
            };
            match relate(relation, &area(node), &area(other_node), tolerance) {
                Ok(()) => report.passed.push(format!("{name} {relation} {other_name}")),
                Err(why) => report.failures.push(format!(
                    "expected {name} {relation} {other_name} (tolerance {tolerance} px), found {why}"
                )),
            }
        }
    }

    if report.failures.is_empty() {
        Ok(format!("✓ {}", report.passed.join("; ")))
    } else {
        Err(report.failures.join("\n"))
    }
}

#[derive(Default)]
struct Report {
    passed: Vec<String>,
    failures: Vec<String>,
}

/// Whether the text in `node` (itself, or the text under it) fits, as the
/// bridge measured it.
fn fit(snapshot: &Snapshot, node: &Node, want: bool) -> (bool, String) {
    let texts: Vec<&Node> = if node.kind == NodeKind::Text {
        vec![node]
    } else {
        snapshot
            .nodes
            .iter()
            .filter(|n| {
                n.kind == NodeKind::Text
                    && n.layer == node.layer
                    && std::iter::successors(n.parent, |&p| {
                        snapshot.nodes.get(p).and_then(|x| x.parent)
                    })
                    .any(|p| p == node.index)
            })
            .collect()
    };
    if texts.is_empty() {
        return (false, "no text in it to measure".into());
    }
    let unmeasured: Vec<&Node> = texts
        .iter()
        .copied()
        .filter(|n| n.text_fit.is_none())
        .collect();
    let overflowing: Vec<String> = texts
        .iter()
        .filter_map(|n| {
            let fit = n.text_fit.filter(|f| !f.fits())?;
            Some(format!(
                "{} needs {:.0}×{:.0} px but has {}×{}",
                view::line(n),
                fit.needed.width,
                fit.needed.height,
                fit.available_width
                    .map_or("∞".into(), |w| format!("{w:.0}")),
                fit.available_height
                    .map_or("∞".into(), |h| format!("{h:.0}")),
            ))
        })
        .collect();
    if !overflowing.is_empty() {
        return (
            !want,
            format!("text that does not fit: {}", overflowing.join("; ")),
        );
    }
    if !unmeasured.is_empty() {
        let layer = if node.layer == Layer::Overlay {
            " (text in overlays is not measured)"
        } else {
            ""
        };
        return (
            false,
            format!(
                "cannot tell: {} is not a measured iced text widget{layer}",
                view::line(unmeasured[0])
            ),
        );
    }
    (want, "all of its text fits".into())
}

/// Checks one geometric relation of `a` to `b`, with `tol` px of slack.
pub(crate) fn relate(relation: &str, a: &Rect, b: &Rect, tol: f32) -> Result<(), String> {
    let (ar, ab) = (a.x + a.width, a.y + a.height);
    let (br, bb) = (b.x + b.width, b.y + b.height);
    let (acx, acy) = a.center();
    let (bcx, bcy) = b.center();
    let edges = |what: &str, value: f32, other: &str, against: f32| {
        format!(
            "its {what} at {value:.0} and the other's {other} at {against:.0} ({:.0} px apart)",
            (value - against).abs()
        )
    };
    let (ok, why) = match relation {
        "left_of" => (ar <= b.x + tol, edges("right edge", ar, "left edge", b.x)),
        "right_of" => (a.x >= br - tol, edges("left edge", a.x, "right edge", br)),
        "above" => (ab <= b.y + tol, edges("bottom edge", ab, "top edge", b.y)),
        "below" => (a.y >= bb - tol, edges("top edge", a.y, "bottom edge", bb)),
        "inside" => (
            a.x >= b.x - tol && a.y >= b.y - tol && ar <= br + tol && ab <= bb + tol,
            format!(
                "it at {} and the other at {}",
                view::rect_text(a),
                view::rect_text(b)
            ),
        ),
        "same_row" => (
            (acy - bcy).abs() <= tol,
            edges("vertical centre", acy, "vertical centre", bcy),
        ),
        "same_column" => (
            (acx - bcx).abs() <= tol,
            edges("horizontal centre", acx, "horizontal centre", bcx),
        ),
        "aligned_left" => (
            (a.x - b.x).abs() <= tol,
            edges("left edge", a.x, "left edge", b.x),
        ),
        "aligned_right" => (
            (ar - br).abs() <= tol,
            edges("right edge", ar, "right edge", br),
        ),
        "aligned_top" => (
            (a.y - b.y).abs() <= tol,
            edges("top edge", a.y, "top edge", b.y),
        ),
        "aligned_bottom" => (
            (ab - bb).abs() <= tol,
            edges("bottom edge", ab, "bottom edge", bb),
        ),
        other => return Err(format!("unknown relation {other}")),
    };
    if ok {
        Ok(())
    } else {
        Err(why)
    }
}

/// An RGBA colour from `#rgb`, `#rrggbb` or `#rrggbbaa`.
pub(crate) fn parse_color(text: &str) -> Result<[u8; 4], String> {
    let hex = text.trim().trim_start_matches('#');
    let digit = |i: usize, len: usize| u8::from_str_radix(&hex[i..i + len], 16);
    let parsed = match hex.len() {
        3 => (0..3)
            .map(|i| digit(i, 1).map(|v| v * 17))
            .chain(std::iter::once(Ok(255)))
            .collect::<Result<Vec<u8>, _>>(),
        6 | 8 => (0..hex.len() / 2)
            .map(|i| digit(i * 2, 2))
            .chain((hex.len() == 6).then_some(Ok(255)))
            .collect(),
        _ => return Err(format!("colour {text:?} is not #rgb, #rrggbb or #rrggbbaa")),
    };
    let values = parsed.map_err(|_| format!("colour {text:?} is not hexadecimal"))?;
    Ok([values[0], values[1], values[2], values[3]])
}

pub(crate) fn hex(rgba: [u8; 4]) -> String {
    let mut out = String::from("#");
    for channel in &rgba[..3] {
        let _ = write!(out, "{channel:02x}");
    }
    if rgba[3] != 255 {
        let _ = write!(out, "{:02x}", rgba[3]);
    }
    out
}

/// Whether `got` is within `tolerance` of `want` on every channel; the
/// largest difference otherwise.
pub(crate) fn same_color(got: [u8; 4], want: [u8; 4], tolerance: u8) -> Result<(), (usize, u8)> {
    let (channel, diff) = got
        .iter()
        .zip(want)
        .map(|(g, w)| g.abs_diff(w))
        .enumerate()
        .fold(
            (0, 0),
            |best, (channel, d)| if d > best.1 { (channel, d) } else { best },
        );
    if diff <= tolerance {
        Ok(())
    } else {
        Err((channel, diff))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_mcp_protocol::{Size, TextFit};

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
            "index": index, "kind": "text", "layer": "base",
            "bounds": {"x": 0, "y": 0, "width": 0, "height": 0},
        }))
        .unwrap();
        node.parent = parent;
        node.kind = kind;
        node.bounds = r;
        node.visible = Some(r);
        node
    }

    /// A form: a column with a name input, Save and Cancel side by side,
    /// and a list of two items under a scrollable.
    fn form() -> Snapshot {
        let mut nodes = vec![node(
            0,
            None,
            NodeKind::Container,
            rect(0.0, 0.0, 400.0, 300.0),
        )];
        let mut input = node(
            1,
            Some(0),
            NodeKind::TextInput,
            rect(10.0, 10.0, 200.0, 30.0),
        );
        input.id = Some("name".into());
        input.text = Some("Ada".into());
        input.focused = Some(true);
        input.interaction = Some("text".into());
        nodes.push(input);
        let mut save = node(2, Some(0), NodeKind::Text, rect(10.0, 50.0, 60.0, 20.0));
        save.text = Some("Save".into());
        save.interaction = Some("pointer".into());
        save.text_fit = Some(TextFit {
            needed: Size {
                width: 40.0,
                height: 20.0,
            },
            available_width: Some(60.0),
            available_height: None,
        });
        nodes.push(save);
        let mut cancel = node(3, Some(0), NodeKind::Text, rect(80.0, 51.0, 60.0, 20.0));
        cancel.text = Some("Cancel (disabled)".into());
        cancel.text_fit = Some(TextFit {
            needed: Size {
                width: 130.0,
                height: 20.0,
            },
            available_width: Some(60.0),
            available_height: None,
        });
        nodes.push(cancel);
        let mut list = node(
            4,
            Some(0),
            NodeKind::Scrollable,
            rect(10.0, 80.0, 200.0, 100.0),
        );
        list.id = Some("list".into());
        nodes.push(list);
        nodes.push(node(
            5,
            Some(4),
            NodeKind::Container,
            rect(10.0, 80.0, 200.0, 400.0),
        ));
        let mut one = node(6, Some(5), NodeKind::Text, rect(10.0, 80.0, 50.0, 20.0));
        one.text = Some("one".into());
        nodes.push(one);
        let mut two = node(7, Some(5), NodeKind::Text, rect(10.0, 380.0, 50.0, 20.0));
        two.text = Some("two".into());
        two.visible = None;
        nodes.push(two);
        Snapshot {
            window: "1".into(),
            size: Size {
                width: 400.0,
                height: 300.0,
            },
            nodes,
        }
    }

    fn expect(json: serde_json::Value) -> Result<String, String> {
        let params: ExpectParams = serde_json::from_value(json).unwrap();
        evaluate(&form(), &params)
    }

    #[test]
    fn presence_visibility_and_count() {
        assert!(
            expect(serde_json::json!({"id": "name"})).is_ok(),
            "exists by default"
        );
        let error = expect(serde_json::json!({"id": "nope"})).unwrap_err();
        assert_eq!(error, "expected #nope to exist, found nothing");
        assert!(expect(serde_json::json!({"id": "nope", "exists": false})).is_ok());
        // Scrolled out: it exists but is not visible.
        assert!(expect(serde_json::json!({"text": "two", "exists": true})).is_ok());
        assert!(expect(serde_json::json!({"text": "two", "visible": false})).is_ok());
        let error = expect(serde_json::json!({"text": "two", "visible": true})).unwrap_err();
        assert!(
            error.starts_with("expected \"two\" to be visible, found it hidden"),
            "{error}"
        );
        assert!(
            expect(serde_json::json!({"text": "o", "count": 1})).is_ok(),
            "visible only"
        );
    }

    #[test]
    fn state_and_text_checks_say_what_they_found() {
        assert!(expect(
            serde_json::json!({"id": "name", "focused": true, "enabled": true, "value": "Ada"})
        )
        .is_ok());
        let error =
            expect(serde_json::json!({"text": "Save", "text_equals": "Saved", "focused": true}))
                .unwrap_err();
        assert_eq!(
            error,
            "expected \"Save\" to be focused, found it not focused: [2] text \"Save\" (10,50 60×20) pointer\nexpected \"Save\" to have text \"Saved\", found \"Save\""
        );
        let error = expect(serde_json::json!({"text": "Cancel", "enabled": true})).unwrap_err();
        assert!(
            error.contains("found it disabled (judged by its cursor"),
            "{error}"
        );
        assert!(expect(serde_json::json!({"text": "Cancel", "text_contains": "DISABLED"})).is_ok());
    }

    #[test]
    fn relations_with_tolerance() {
        assert!(expect(serde_json::json!({"text": "Save", "left_of": {"text": "Cancel"}, "same_row": {"text": "Cancel"}, "below": {"id": "name"}, "aligned_left": {"id": "name"}, "inside": {"ref": 0}})).is_ok());
        let error =
            expect(serde_json::json!({"text": "Cancel", "left_of": {"text": "Save"}})).unwrap_err();
        assert_eq!(
            error,
            "expected \"Cancel\" left_of \"Save\" (tolerance 2 px), found its right edge at 140 and the other's left edge at 10 (130 px apart)"
        );
        // One pixel off is within the default tolerance, not within 0.5.
        assert!(
            expect(serde_json::json!({"text": "Save", "aligned_top": {"text": "Cancel"}})).is_ok()
        );
        assert!(expect(
            serde_json::json!({"text": "Save", "aligned_top": {"text": "Cancel"}, "tolerance": 0.5})
        )
        .is_err());
        let error =
            expect(serde_json::json!({"text": "Save", "above": {"id": "ghost"}})).unwrap_err();
        assert_eq!(error, "expected \"Save\" above #ghost, found no #ghost");
    }

    #[test]
    fn fits_children_and_values() {
        assert!(expect(serde_json::json!({"text": "Save", "fits": true})).is_ok());
        let error = expect(serde_json::json!({"text": "Cancel", "fits": true})).unwrap_err();
        assert!(error.contains("needs 130×20 px but has 60×∞"), "{error}");
        assert!(
            expect(serde_json::json!({"ref": 0, "fits": false})).is_ok(),
            "some text under it overflows"
        );
        let error = expect(serde_json::json!({"id": "name", "fits": true})).unwrap_err();
        assert!(error.contains("no text in it to measure"), "{error}");
        assert!(
            expect(serde_json::json!({"id": "list", "children": 2})).is_ok(),
            "through the column, hidden included"
        );
        let error = expect(serde_json::json!({"id": "list", "children": 3})).unwrap_err();
        assert!(
            error.starts_with("expected #list to have 3 child(ren), found 2: [6] text \"one\""),
            "{error}"
        );
        let error = expect(serde_json::json!({"text": "Save", "value": "x"})).unwrap_err();
        assert!(error.contains("found no value reported"), "{error}");
    }

    #[test]
    fn colours_parse_and_compare() {
        assert_eq!(parse_color("#fff").unwrap(), [255, 255, 255, 255]);
        assert_eq!(parse_color("1e1e2a").unwrap(), [0x1e, 0x1e, 0x2a, 255]);
        assert_eq!(parse_color("#00000080").unwrap(), [0, 0, 0, 0x80]);
        assert!(parse_color("#12").is_err());
        assert!(parse_color("#zzzzzz").is_err());
        assert_eq!(hex([0x1e, 0x1e, 0x2a, 255]), "#1e1e2a");
        assert!(same_color([10, 20, 30, 255], [12, 18, 30, 255], 2).is_ok());
        assert_eq!(
            same_color([10, 20, 30, 255], [10, 20, 90, 255], 8),
            Err((2, 60))
        );
    }
}
