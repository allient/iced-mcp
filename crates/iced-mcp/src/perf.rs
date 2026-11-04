//! Timings for the `perf` tool: the app's `update` per message kind, its
//! `view`, and the interval between rendered frames.
//!
//! Cheap enough to stay on: a duration push per call, bounded memory.

use iced_mcp_protocol::{KindTiming, PerfReport, Timing};

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Samples kept per series for percentiles; counts and maxima cover all.
const KEPT: usize = 4096;
/// Message kinds reported, costliest first.
const SLOWEST: usize = 10;
/// A gap longer than this between redraws is idling, not a slow frame.
const IDLE_GAP: Duration = Duration::from_millis(250);

#[derive(Default)]
pub(crate) struct Series {
    count: u64,
    sum_us: u64,
    max_us: u64,
    kept: Vec<u32>,
    next: usize,
}

impl Series {
    pub(crate) fn push(&mut self, duration: Duration) {
        let us = u64::try_from(duration.as_micros()).unwrap_or(u64::MAX);
        self.count += 1;
        self.sum_us = self.sum_us.saturating_add(us);
        self.max_us = self.max_us.max(us);
        let sample = u32::try_from(us).unwrap_or(u32::MAX);
        if self.kept.len() < KEPT {
            self.kept.push(sample);
        } else {
            self.kept[self.next] = sample;
            self.next = (self.next + 1) % KEPT;
        }
    }

    fn timing(&self) -> Timing {
        let mut sorted = self.kept.clone();
        sorted.sort_unstable();
        let p95 = sorted
            .get((sorted.len() * 95 / 100).min(sorted.len().saturating_sub(1)))
            .copied()
            .unwrap_or(0);
        Timing {
            count: self.count,
            mean_us: self.sum_us.checked_div(self.count).unwrap_or(0),
            p95_us: u64::from(p95),
            max_us: self.max_us,
        }
    }
}

pub(crate) struct Perf {
    started: Instant,
    pub(crate) view: Series,
    update: Series,
    kinds: HashMap<String, (u64, u64, u64)>,
    frames: Series,
    last_frame: Option<Instant>,
}

impl Default for Perf {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            view: Series::default(),
            update: Series::default(),
            kinds: HashMap::new(),
            frames: Series::default(),
            last_frame: None,
        }
    }
}

impl Perf {
    pub(crate) fn update(&mut self, kind: &str, duration: Duration) {
        self.update.push(duration);
        let us = u64::try_from(duration.as_micros()).unwrap_or(u64::MAX);
        let entry = self.kinds.entry(kind.to_owned()).or_insert((0, 0, 0));
        entry.0 += 1;
        entry.1 = entry.1.saturating_add(us);
        entry.2 = entry.2.max(us);
    }

    /// A window redrew at `at`.
    pub(crate) fn frame(&mut self, at: Instant) {
        if let Some(last) = self.last_frame.replace(at) {
            let gap = at.saturating_duration_since(last);
            if gap < IDLE_GAP {
                self.frames.push(gap);
            }
        }
    }

    pub(crate) fn report(&self) -> PerfReport {
        let mut slowest: Vec<KindTiming> = self
            .kinds
            .iter()
            .map(|(kind, (count, total_us, max_us))| KindTiming {
                kind: kind.clone(),
                count: *count,
                total_us: *total_us,
                max_us: *max_us,
            })
            .collect();
        slowest.sort_by_key(|k| std::cmp::Reverse(k.total_us));
        slowest.truncate(SLOWEST);
        PerfReport {
            span_ms: u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            frames: self.frames.timing(),
            view: self.view.timing(),
            update: self.update.timing(),
            slowest,
        }
    }
}

/// `Chart(Book(Widget(..), ..))` → `Chart(Book`: a message's variant path,
/// two levels deep, which is what groups its timings.
pub(crate) fn kind(text: &str) -> &str {
    let stop = |c: char| matches!(c, '(' | '{' | ' ' | ',' | ')');
    let Some(first) = text.find(stop) else {
        return text;
    };
    let rest = &text[first + 1..];
    // Go one level in only for a nested variant: `Chart(Book`, not `Toggle(0`.
    if text[first..].starts_with('(') && rest.starts_with(|c: char| c.is_ascii_uppercase()) {
        let second = rest.find(stop).map_or(text.len(), |j| first + 1 + j);
        return &text[..second];
    }
    &text[..first]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_are_variant_paths() {
        assert_eq!(kind("Chart(Book(W(\"a\"), [1.0]))"), "Chart(Book");
        assert_eq!(kind("SaveTick"), "SaveTick");
        assert_eq!(kind("Opened { id: 1 }"), "Opened");
        assert_eq!(kind("Toggle(0, true)"), "Toggle");
    }

    #[test]
    fn report_ranks_kinds_by_total_time() {
        let mut perf = Perf::default();
        perf.update("Fast", Duration::from_micros(10));
        perf.update("Slow", Duration::from_micros(900));
        perf.update("Fast", Duration::from_micros(10));
        let report = perf.report();
        assert_eq!(report.update.count, 3);
        assert_eq!(report.slowest[0].kind, "Slow");
        assert_eq!(report.slowest[1].count, 2);
        assert_eq!(report.update.max_us, 900);
    }

    #[test]
    fn idle_gaps_are_not_frames() {
        let mut perf = Perf::default();
        let t = Instant::now();
        perf.frame(t);
        perf.frame(t + Duration::from_millis(16));
        perf.frame(t + Duration::from_secs(5));
        assert_eq!(perf.report().frames.count, 1);
    }
}
