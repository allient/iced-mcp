//! The widget wrapped around each window's view. It draws and lays out
//! exactly like the app's own root, and on every update it serves the
//! window's queued jobs from inside the widget tree:
//!
//! - **snapshot**: runs a [`Collector`] over the base and the overlay;
//! - **probe**: asks both layers what cursor they show at a point;
//! - **act**: delivers synthetic events, one frame of them per update.
//!
//! iced hands each event to the overlay (menus, dropdowns, dialogs) before the
//! base, and hides the cursor from the base while it is over the overlay; the
//! two passes below repeat that routing, so synthetic input lands where real
//! input would. The overlay pass leaves its half in [`Slot::pass`] for the
//! base pass to finish and answer.

use crate::collect::{covered, id_name, interaction_name, probe_points, regions, Collector};
use crate::registry::{Job, Pass, Registry, Slot};

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay::{self, Nested};
use iced::advanced::renderer;
use iced::advanced::widget::operation::{Focusable, TextInput};
use iced::advanced::widget::{tree, Id, Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::mouse::{Cursor, Interaction};
use iced::window::RedrawRequest;
use iced::{window, Element, Event, Length, Point, Rectangle, Size, Vector};
use iced_mcp_protocol::{Layer, Node, Outcome, Probe, Response, Snapshot, WidgetOperationKind};

use std::cell::RefCell;

pub(crate) struct Inspector<'a, Message, Theme, Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
    registry: Registry,
    window: window::Id,
}

struct TargetWidgetOperation {
    id: String,
    operation: WidgetOperationKind,
    found: bool,
}

impl Operation for TargetWidgetOperation {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn focusable(&mut self, id: Option<&Id>, _bounds: Rectangle, state: &mut dyn Focusable) {
        if matches!(self.operation, WidgetOperationKind::Focus)
            && id.and_then(id_name).as_deref() == Some(self.id.as_str())
        {
            state.focus();
            self.found = true;
        }
    }

    fn text_input(&mut self, id: Option<&Id>, _bounds: Rectangle, state: &mut dyn TextInput) {
        if matches!(self.operation, WidgetOperationKind::SelectAll)
            && id.and_then(id_name).as_deref() == Some(self.id.as_str())
        {
            state.select_all();
            self.found = true;
        }
    }
}

impl<'a, Message, Theme, Renderer> Inspector<'a, Message, Theme, Renderer> {
    pub(crate) fn new(
        content: Element<'a, Message, Theme, Renderer>,
        registry: Registry,
        window: window::Id,
    ) -> Self {
        Self {
            content,
            registry,
            window,
        }
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for Inspector<'_, Message, Theme, Renderer>
where
    Renderer: iced::advanced::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::stateless()
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
        draw_highlight(&self.registry, self.window, renderer);
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );

        let registry = self.registry.clone();
        let mut windows = registry.lock();
        if let Event::Window(window::Event::RedrawRequested(at)) = event {
            windows.perf.frame(*at);
        }
        let slot = windows.slot(self.window);
        // The viewport is the window; the root's layout may be smaller.
        slot.size = viewport.size();
        self.serve(
            slot,
            &mut tree.children[0],
            layout,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        let raw = self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )?;

        Some(overlay::Element::new(Box::new(OverlayInspector {
            nested: RefCell::new(Nested::new(raw)),
            registry: self.registry.clone(),
            window: self.window,
        })))
    }
}

impl<Message, Theme, Renderer> Inspector<'_, Message, Theme, Renderer>
where
    Renderer: iced::advanced::Renderer,
{
    /// The base pass: finishes the job at the head of the queue.
    #[allow(clippy::too_many_arguments)]
    fn serve(
        &mut self,
        slot: &mut Slot,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let pass = slot.pass.take();
        let Some(job) = slot.jobs.front_mut() else {
            return;
        };
        let content = &mut self.content;

        let finished = match job {
            Job::Snapshot { grid, reply } => {
                let mut collector = Collector::new(Layer::Base);
                content
                    .as_widget_mut()
                    .operate(tree, layout, renderer, &mut collector);
                let mut nodes = collector.nodes;
                let mut hit = |point: Point| {
                    content.as_widget().mouse_interaction(
                        tree,
                        layout,
                        Cursor::Available(point),
                        viewport,
                        renderer,
                    )
                };
                for (index, point) in probe_points(&nodes) {
                    if let Some(node) = nodes.get_mut(index) {
                        node.interaction = interaction_name(hit(point));
                    }
                }
                let overlay = match pass {
                    Some(Pass::Snapshot(overlay)) => overlay,
                    _ => Vec::new(),
                };
                if *grid > 0.0 {
                    // The overlay is on top: whatever it covers is not the base's.
                    let mut covered = covered(&nodes);
                    covered.extend(overlay.iter().filter_map(|n| n.visible));
                    let regions = regions(
                        &mut hit,
                        *viewport,
                        *grid,
                        &covered,
                        Layer::Base,
                        nodes.len(),
                    );
                    nodes.extend(regions);
                }
                append(&mut nodes, overlay);
                let _ = reply.try_send(Outcome::Ok(Response::Snapshot(Snapshot {
                    window: self.window.to_string(),
                    size: size(slot.size),
                    nodes,
                })));
                true
            }
            Job::Probe { point, reply } => {
                let (interaction, layer) = match pass {
                    Some(Pass::Probe(Some(interaction))) => (interaction, Layer::Overlay),
                    _ => (
                        content.as_widget().mouse_interaction(
                            tree,
                            layout,
                            Cursor::Available(*point),
                            viewport,
                            renderer,
                        ),
                        Layer::Base,
                    ),
                };
                let _ = reply.try_send(Outcome::Ok(Response::Probe(Probe {
                    x: point.x,
                    y: point.y,
                    interaction: interaction_name(interaction).unwrap_or_else(|| "none".into()),
                    layer,
                })));
                true
            }
            Job::WidgetOperation {
                id,
                operation,
                reply,
            } => {
                let overlay_found = match pass {
                    Some(Pass::WidgetOperation(found)) => found,
                    _ => false,
                };
                let found = if overlay_found {
                    true
                } else {
                    let mut operation = TargetWidgetOperation {
                        id: id.clone(),
                        operation: *operation,
                        found: false,
                    };
                    content
                        .as_widget_mut()
                        .operate(tree, layout, renderer, &mut operation);
                    operation.found
                };
                let _ = reply.try_send(Outcome::Ok(Response::WidgetOperation { found }));
                true
            }
            Job::Act {
                frames,
                acted,
                reply,
            } => {
                let (events, captured, covers_cursor, overlay_messages) = match pass {
                    Some(Pass::Act {
                        events,
                        captured,
                        covers_cursor,
                        messages,
                    }) => (events, captured, covers_cursor, messages),
                    _ => {
                        let inputs = frames.pop_front().unwrap_or_default();
                        let events = crate::input::events(inputs, &mut slot.cursor);
                        let captured = vec![false; events.len()];
                        (events, captured, false, 0)
                    }
                };

                acted.messages += overlay_messages;
                for ((event, at), taken) in events.iter().zip(captured) {
                    if taken {
                        acted.captured += 1;
                        continue;
                    }
                    let at = if covers_cursor {
                        Cursor::Unavailable
                    } else {
                        *at
                    };
                    let (messages, was_captured) = forward(shell, |inner| {
                        content.as_widget_mut().update(
                            tree, event, layout, at, renderer, clipboard, inner, viewport,
                        );
                    });
                    acted.messages += messages;
                    acted.captured += usize::from(was_captured);
                }
                acted.frames += 1;

                if frames.is_empty() {
                    let _ = reply.try_send(Outcome::Ok(Response::Acted(acted.clone())));
                    true
                } else {
                    false
                }
            }
        };

        if finished {
            slot.jobs.pop_front();
        }
        if !slot.jobs.is_empty() {
            shell.request_redraw();
        }
    }
}

fn draw_highlight<Renderer: iced::advanced::Renderer>(
    registry: &Registry,
    window: window::Id,
    renderer: &mut Renderer,
) {
    let bounds = {
        let mut windows = registry.lock();
        let slot = windows.slot(window);
        match slot.highlight {
            Some(highlight) if highlight.expires_at > std::time::Instant::now() => {
                Some(highlight.bounds)
            }
            Some(_) => {
                slot.highlight = None;
                None
            }
            None => None,
        }
    };
    let Some(bounds) = bounds else {
        return;
    };
    renderer.fill_quad(
        renderer::Quad {
            bounds: Rectangle {
                x: bounds.x,
                y: bounds.y,
                width: bounds.width,
                height: bounds.height,
            },
            border: iced::Border::default()
                .color(iced::Color::from_rgb(1.0, 0.42, 0.08))
                .width(3.0)
                .rounded(3.0),
            ..renderer::Quad::default()
        },
        iced::Color::TRANSPARENT,
    );
}

/// The overlay half of the work, wrapped around whatever the content opened.
/// It nests the overlay itself, so menus that open submenus are covered too.
struct OverlayInspector<'a, Message, Theme, Renderer> {
    nested: RefCell<Nested<'a, Message, Theme, Renderer>>,
    registry: Registry,
    window: window::Id,
}

impl<Message, Theme, Renderer> overlay::Overlay<Message, Theme, Renderer>
    for OverlayInspector<'_, Message, Theme, Renderer>
where
    Renderer: iced::advanced::Renderer,
{
    fn layout(&mut self, renderer: &Renderer, bounds: Size) -> layout::Node {
        self.nested.get_mut().layout(renderer, bounds)
    }

    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: Cursor,
    ) {
        self.nested
            .borrow_mut()
            .draw(renderer, theme, style, layout, cursor);
        draw_highlight(&self.registry, self.window, renderer);
    }

    fn operate(&mut self, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation) {
        self.nested.get_mut().operate(layout, renderer, operation);
    }

    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        let nested = self.nested.get_mut();
        nested.update(event, layout, cursor, renderer, clipboard, shell);

        let mut windows = self.registry.lock();
        let slot = windows.slot(self.window);
        // Once per frame: the base pass clears it.
        if slot.pass.is_some() {
            return;
        }
        let Some(job) = slot.jobs.front_mut() else {
            return;
        };

        let pass = match job {
            Job::Snapshot { grid, .. } => {
                let mut collector = Collector::new(Layer::Overlay);
                nested.operate(layout, renderer, &mut collector);
                let mut nodes = collector.nodes;
                for (index, point) in probe_points(&nodes) {
                    let interaction =
                        nested.mouse_interaction(layout, Cursor::Available(point), renderer);
                    if let Some(node) = nodes.get_mut(index) {
                        node.interaction = interaction_name(interaction);
                    }
                }
                if *grid > 0.0 {
                    let covered = covered(&nodes);
                    let window = Rectangle::with_size(slot.size);
                    let mut hit = |point: Point| {
                        nested.mouse_interaction(layout, Cursor::Available(point), renderer)
                    };
                    let found = regions(
                        &mut hit,
                        window,
                        *grid,
                        &covered,
                        Layer::Overlay,
                        nodes.len(),
                    );
                    nodes.extend(found);
                }
                Pass::Snapshot(nodes)
            }
            Job::Probe { point, .. } => {
                let interaction =
                    nested.mouse_interaction(layout, Cursor::Available(*point), renderer);
                Pass::Probe((interaction != Interaction::None).then_some(interaction))
            }
            Job::WidgetOperation { id, operation, .. } => {
                let mut target = TargetWidgetOperation {
                    id: id.clone(),
                    operation: *operation,
                    found: false,
                };
                nested.operate(layout, renderer, &mut target);
                Pass::WidgetOperation(target.found)
            }
            Job::Act { frames, .. } => {
                let inputs = frames.pop_front().unwrap_or_default();
                let events = crate::input::events(inputs, &mut slot.cursor);
                let mut captured = Vec::with_capacity(events.len());
                let mut messages = 0;
                for (event, at) in &events {
                    let (published, was_captured) = forward(shell, |inner| {
                        nested.update(event, layout, *at, renderer, clipboard, inner);
                    });
                    messages += published;
                    captured.push(was_captured);
                }
                let covers_cursor = slot.cursor.is_some_and(|point: Point| {
                    nested.mouse_interaction(layout, Cursor::Available(point), renderer)
                        != Interaction::None
                });
                Pass::Act {
                    events,
                    captured,
                    covers_cursor,
                    messages,
                }
            }
        };
        slot.pass = Some(pass);
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: Cursor,
        renderer: &Renderer,
    ) -> Interaction {
        self.nested
            .borrow_mut()
            .mouse_interaction(layout, cursor, renderer)
    }
}

/// Runs `dispatch` against a shell of its own and forwards what it produced
/// to `outer` — messages, redraws, relayouts — but not the event status: a
/// synthetic event captured by a widget must not swallow the real event this
/// update is carrying. Returns the messages published and whether the
/// synthetic event was captured.
fn forward<Message>(
    outer: &mut Shell<'_, Message>,
    dispatch: impl FnOnce(&mut Shell<'_, Message>),
) -> (usize, bool) {
    let mut messages = Vec::new();
    let captured = {
        let mut inner = Shell::new(&mut messages);
        dispatch(&mut inner);
        if inner.is_layout_invalid() {
            outer.invalidate_layout();
        }
        if inner.are_widgets_invalid() {
            outer.invalidate_widgets();
        }
        match inner.redraw_request() {
            RedrawRequest::NextFrame => outer.request_redraw(),
            RedrawRequest::At(at) => outer.request_redraw_at(at),
            RedrawRequest::Wait => {}
        }
        outer.input_method_mut().merge(inner.input_method());
        inner.is_event_captured()
    };
    let published = messages.len();
    for message in messages {
        outer.publish(message);
    }
    (published, captured)
}

/// Appends the overlay's nodes after the base's, re-indexed.
fn append(nodes: &mut Vec<Node>, overlay: Vec<Node>) {
    let offset = nodes.len();
    nodes.extend(overlay.into_iter().map(|mut node| {
        node.index += offset;
        node.parent = node.parent.map(|parent| parent + offset);
        node
    }));
}

fn size(size: Size) -> iced_mcp_protocol::Size {
    iced_mcp_protocol::Size {
        width: size.width,
        height: size.height,
    }
}

impl<'a, Message, Theme, Renderer> From<Inspector<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: iced::advanced::Renderer + 'a,
{
    fn from(inspector: Inspector<'a, Message, Theme, Renderer>) -> Self {
        Element::new(inspector)
    }
}
