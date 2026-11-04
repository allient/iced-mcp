//! A ring buffer of the app's messages in their `Debug` form — what the
//! agent reads to see how the app reacted to its input.

use iced_mcp_protocol::{MessageEntry, MessageLog, MessagesParams};

use std::collections::VecDeque;
use std::fmt::{self, Write as _};
use std::time::Instant;

/// `Debug` output past this is cut; formatting stops there, so a message
/// carrying a large payload costs no more than a small one.
const MAX_TEXT: usize = 400;
const DEFAULT_LIMIT: usize = 50;

pub(crate) struct Log {
    entries: VecDeque<MessageEntry>,
    capacity: usize,
    next: u64,
    dropped: u64,
    started: Instant,
}

impl Log {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            entries: VecDeque::with_capacity(capacity.min(4096)),
            capacity,
            next: 1,
            dropped: 0,
            started: Instant::now(),
        }
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.capacity > 0
    }

    #[cfg(test)]
    pub(crate) fn record(&mut self, message: &impl fmt::Debug) {
        if self.capacity > 0 {
            self.push(describe(message));
        }
    }

    /// Records a message already described with [`describe`].
    pub(crate) fn push(&mut self, text: String) {
        if self.capacity == 0 {
            return;
        }
        if self.entries.len() == self.capacity {
            self.entries.pop_front();
            self.dropped += 1;
        }
        self.entries.push_back(MessageEntry {
            seq: self.next,
            at_ms: u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            text,
        });
        self.next += 1;
    }

    pub(crate) fn read(&self, params: &MessagesParams) -> MessageLog {
        let since = params.since.unwrap_or(0);
        let limit = params.limit.unwrap_or(DEFAULT_LIMIT).max(1);
        let filter = params.filter.as_deref().map(str::to_lowercase);
        let matching: Vec<&MessageEntry> = self
            .entries
            .iter()
            .filter(|entry| entry.seq > since)
            .filter(|entry| {
                filter
                    .as_deref()
                    .is_none_or(|f| entry.text.to_lowercase().contains(f))
            })
            .collect();
        // The newest `limit`, oldest first.
        let skip = matching.len().saturating_sub(limit);
        MessageLog {
            entries: matching.into_iter().skip(skip).cloned().collect(),
            last: self.next - 1,
            dropped: self.dropped,
        }
    }
}

/// A message's `Debug` text, cut at [`MAX_TEXT`] without formatting the rest.
pub(crate) fn describe(message: &impl fmt::Debug) -> String {
    let mut text = Bounded(String::new());
    if write!(text, "{message:?}").is_err() {
        text.0.push('…');
    }
    text.0
}

/// A `fmt::Write` that refuses to grow past [`MAX_TEXT`] bytes.
struct Bounded(String);

impl fmt::Write for Bounded {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let room = MAX_TEXT.saturating_sub(self.0.len());
        if s.len() <= room {
            self.0.push_str(s);
            return Ok(());
        }
        let mut cut = room;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        self.0.push_str(&s[..cut]);
        Err(fmt::Error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_newest_and_counts_the_dropped() {
        let mut log = Log::new(3);
        for i in 0..5 {
            log.record(&i);
        }
        let read = log.read(&MessagesParams::default());
        let texts: Vec<_> = read.entries.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, ["2", "3", "4"]);
        assert_eq!(read.dropped, 2);
        assert_eq!(read.last, 5);
    }

    #[test]
    fn since_and_filter_narrow_the_read() {
        let mut log = Log::new(10);
        log.record(&"Save");
        log.record(&"Open");
        log.record(&"SaveAs");
        let read = log.read(&MessagesParams {
            since: Some(1),
            filter: Some("save".into()),
            limit: None,
        });
        assert_eq!(read.entries.len(), 1);
        assert_eq!(read.entries[0].seq, 3);
    }

    #[test]
    fn huge_messages_are_cut() {
        let mut log = Log::new(1);
        log.record(&"x".repeat(10_000));
        let read = log.read(&MessagesParams::default());
        assert!(read.entries[0].text.len() <= MAX_TEXT + '…'.len_utf8());
    }
}
