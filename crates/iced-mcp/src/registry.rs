//! Work queued for each window, shared between the program (which receives
//! requests) and the window's [`Inspector`](crate::inspector::Inspector)
//! (which does the work inside the widget tree, frame by frame).

use iced::mouse::{Cursor, Interaction};
use iced::{window, Event, Point, Size};
use iced_mcp_protocol::{Acted, Input, Node, Outcome, Rect, WidgetOperationKind};

use std::collections::VecDeque;
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

/// Where a request's answer goes: the connection thread waiting on it.
pub(crate) type Replier = SyncSender<Outcome>;

#[derive(Clone, Default)]
pub(crate) struct Registry(Arc<Mutex<Windows>>);

impl Registry {
    pub(crate) fn lock(&self) -> MutexGuard<'_, Windows> {
        // A panic mid-frame leaves the queues consistent enough to go on.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[derive(Default)]
pub(crate) struct Windows {
    /// In the order they first rendered.
    pub(crate) slots: Vec<Slot>,
    /// Update, view and frame timings, for the `perf` request.
    pub(crate) perf: crate::perf::Perf,
}

impl Windows {
    pub(crate) fn slot(&mut self, id: window::Id) -> &mut Slot {
        let index = match self.slots.iter().position(|slot| slot.id == id) {
            Some(index) => index,
            None => {
                self.slots.push(Slot::new(id));
                self.slots.len() - 1
            }
        };
        &mut self.slots[index]
    }

    /// The window a request names, or the first one when it names none.
    pub(crate) fn find(&mut self, name: Option<&str>) -> Option<&mut Slot> {
        match name {
            Some(name) => self
                .slots
                .iter_mut()
                .find(|slot| slot.id.to_string() == name),
            None => self.slots.first_mut(),
        }
    }

    pub(crate) fn remove(&mut self, id: window::Id) {
        if let Some(index) = self.slots.iter().position(|slot| slot.id == id) {
            let slot = self.slots.remove(index);
            for job in slot.jobs {
                job.fail("the window closed");
            }
        }
    }
}

pub(crate) struct Slot {
    pub(crate) id: window::Id,
    pub(crate) size: Size,
    pub(crate) jobs: VecDeque<Job>,
    /// Where the synthetic cursor is; `None` until the first move.
    pub(crate) cursor: Option<Point>,
    /// What the overlay pass did this frame for the job at the head of the
    /// queue, for the base pass to finish.
    pub(crate) pass: Option<Pass>,
    /// Temporary MCP highlight, drawn above the app content.
    pub(crate) highlight: Option<Highlight>,
}

#[derive(Clone, Copy)]
pub(crate) struct Highlight {
    pub(crate) bounds: Rect,
    pub(crate) expires_at: Instant,
}

impl Slot {
    fn new(id: window::Id) -> Self {
        Self {
            id,
            size: Size::ZERO,
            jobs: VecDeque::new(),
            cursor: None,
            pass: None,
            highlight: None,
        }
    }
}

pub(crate) enum Job {
    Snapshot {
        /// Hit-test grid step for [`NodeKind::Region`]s; `0` skips it.
        ///
        /// [`NodeKind::Region`]: iced_mcp_protocol::NodeKind::Region
        grid: f32,
        reply: Replier,
    },
    Probe {
        point: Point,
        reply: Replier,
    },
    Act {
        frames: VecDeque<Vec<Input>>,
        acted: Acted,
        reply: Replier,
    },
    WidgetOperation {
        id: String,
        operation: WidgetOperationKind,
        reply: Replier,
    },
}

impl Job {
    pub(crate) fn fail(self, error: &str) {
        let reply = match self {
            Job::Snapshot { reply, .. }
            | Job::Probe { reply, .. }
            | Job::Act { reply, .. }
            | Job::WidgetOperation { reply, .. } => reply,
        };
        let _ = reply.try_send(Outcome::Error(error.to_owned()));
    }
}

pub(crate) enum Pass {
    Snapshot(Vec<Node>),
    Probe(Option<Interaction>),
    WidgetOperation(bool),
    Act {
        events: Vec<(Event, Cursor)>,
        /// Per event: whether the overlay consumed it, so the base must not
        /// see it.
        captured: Vec<bool>,
        /// The pointer is over the overlay: the base sees no cursor.
        covers_cursor: bool,
        messages: usize,
    },
}
