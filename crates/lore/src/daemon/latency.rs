//! Request-latency percentiles for the query surface.
//!
//! The bench transcripts showed `search` is bimodal — a ~100 ms body and a
//! multi-second tail parked at the query-embed cap — but client-side timing
//! cannot say *which* stage owns the tail. The daemon therefore times its own
//! stages: `search` (whole handler), `search_embed` (the embed-query wait
//! inside it), and `expand`. `status` reports nearest-rank percentiles over a
//! rolling window so a long-lived daemon reflects current behavior, not its
//! cold start.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use lore_core::EndpointLatency;

/// Samples kept per endpoint. Big enough that p99 over a bench run is real,
/// small enough that snapshotting (copy + sort) stays trivial.
const WINDOW: usize = 4096;

#[derive(Default)]
struct Window {
    /// Lifetime count — keeps growing after the ring starts evicting.
    samples: u64,
    ring: VecDeque<u32>,
}

/// Shared recorder; cheap to clone, locked only for a push or a snapshot.
///
/// Keys are strings because the store stage is recorded **per project**
/// (`search_store:<project>`): the vector arm is an O(n) scan, so a daemon
/// serving one 82k-chunk project and two 4k ones has three different search
/// latencies, and a single aggregate would hide the one that matters.
/// Cardinality is bounded by the registry size.
#[derive(Clone, Default)]
pub struct LatencyRecorder {
    windows: Arc<Mutex<HashMap<String, Window>>>,
}

impl LatencyRecorder {
    pub fn record(&self, endpoint: &str, elapsed: Duration) {
        let ms = u32::try_from(elapsed.as_millis()).unwrap_or(u32::MAX);
        let mut windows = self.windows.lock().expect("latency lock");
        let window = match windows.get_mut(endpoint) {
            Some(window) => window,
            None => windows.entry(endpoint.to_string()).or_default(),
        };
        window.samples += 1;
        if window.ring.len() == WINDOW {
            window.ring.pop_front();
        }
        window.ring.push_back(ms);
    }

    /// Percentiles per endpoint, sorted by endpoint name for stable output.
    pub fn snapshot(&self) -> Vec<EndpointLatency> {
        let windows = self.windows.lock().expect("latency lock");
        let mut out: Vec<EndpointLatency> = windows
            .iter()
            .map(|(endpoint, window)| {
                let mut sorted: Vec<u32> = window.ring.iter().copied().collect();
                sorted.sort_unstable();
                let pct = |p: usize| -> u64 {
                    // Canonical nearest-rank: ceil(p·n/100), 1-indexed. The
                    // window is never empty because it only exists after a
                    // record().
                    let rank = (p * sorted.len()).div_ceil(100).max(1);
                    u64::from(sorted[rank - 1])
                };
                EndpointLatency {
                    endpoint: endpoint.clone(),
                    samples: window.samples,
                    p50_ms: pct(50),
                    p90_ms: pct(90),
                    p95_ms: pct(95),
                    p99_ms: pct(99),
                    max_ms: u64::from(*sorted.last().expect("non-empty window")),
                }
            })
            .collect();
        out.sort_by(|a, b| a.endpoint.cmp(&b.endpoint));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recorder_with(endpoint: &'static str, values: &[u64]) -> LatencyRecorder {
        let recorder = LatencyRecorder::default();
        for v in values {
            recorder.record(endpoint, Duration::from_millis(*v));
        }
        recorder
    }

    #[test]
    fn percentiles_are_nearest_rank_over_the_window() {
        let values: Vec<u64> = (1..=100).collect();
        let snap = recorder_with("search", &values).snapshot();
        assert_eq!(snap.len(), 1);
        let s = &snap[0];
        assert_eq!(
            (s.samples, s.p50_ms, s.p90_ms, s.p95_ms, s.p99_ms, s.max_ms),
            (100, 50, 90, 95, 99, 100),
            "nearest-rank on 1..=100"
        );
    }

    #[test]
    fn a_single_sample_reports_itself_at_every_percentile() {
        let snap = recorder_with("expand", &[7]).snapshot();
        let s = &snap[0];
        assert_eq!((s.p50_ms, s.p99_ms, s.max_ms, s.samples), (7, 7, 7, 1));
    }

    #[test]
    fn the_window_evicts_but_the_lifetime_count_does_not() {
        let recorder = LatencyRecorder::default();
        for _ in 0..(WINDOW + 500) {
            recorder.record("search", Duration::from_millis(1));
        }
        // 500 old samples fell out of the ring; the count keeps the truth.
        let s = &recorder.snapshot()[0];
        assert_eq!(s.samples as usize, WINDOW + 500);
        assert_eq!(s.max_ms, 1);
    }

    #[test]
    fn per_project_store_labels_are_independent_windows() {
        let recorder = LatencyRecorder::default();
        recorder.record("search_store:Lexomancy", Duration::from_millis(1200));
        recorder.record("search_store:lore", Duration::from_millis(30));
        let snap = recorder.snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!((snap[0].endpoint.as_str(), snap[0].p50_ms), ("search_store:Lexomancy", 1200));
        assert_eq!((snap[1].endpoint.as_str(), snap[1].p50_ms), ("search_store:lore", 30));
    }

    #[test]
    fn endpoints_are_independent_and_sorted() {
        let recorder = LatencyRecorder::default();
        recorder.record("search", Duration::from_millis(100));
        recorder.record("expand", Duration::from_millis(5));
        recorder.record("search_embed", Duration::from_millis(90));
        let names: Vec<String> = recorder
            .snapshot()
            .into_iter()
            .map(|e| e.endpoint)
            .collect();
        assert_eq!(names, ["expand", "search", "search_embed"]);
    }
}
