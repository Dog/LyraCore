use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(crate) struct StageTiming {
    count: AtomicU64,
    total_micros: AtomicU64,
    max_micros: AtomicU64,
}

impl StageTiming {
    pub(crate) const fn new() -> Self {
        Self {
            count: AtomicU64::new(0),
            total_micros: AtomicU64::new(0),
            max_micros: AtomicU64::new(0),
        }
    }

    pub(crate) fn record(&self, elapsed: Duration) {
        let micros = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
        self.count.fetch_add(1, Ordering::Relaxed);
        self.total_micros.fetch_add(micros, Ordering::Relaxed);
        self.max_micros.fetch_max(micros, Ordering::Relaxed);
    }

    fn take(&self) -> Window {
        Window {
            count: self.count.swap(0, Ordering::Relaxed),
            total_micros: self.total_micros.swap(0, Ordering::Relaxed),
            max_micros: self.max_micros.swap(0, Ordering::Relaxed),
        }
    }
}

struct Window {
    count: u64,
    total_micros: u64,
    max_micros: u64,
}

impl Window {
    /// Formats `name=<mean>/<max>us(<count>)`.
    fn field(&self, name: &str) -> String {
        format!(
            "{name}={}/{}us({})",
            self.total_micros.checked_div(self.count).unwrap_or(0),
            self.max_micros,
            self.count
        )
    }
}

/// Age of the oldest World Session queue entry when its encoding batch starts.
pub(crate) static QUEUE_WAIT: StageTiming = StageTiming::new();
pub(crate) static ENCODER_PERMIT_WAIT: StageTiming = StageTiming::new();
/// From holding an encoder permit to the blocking worker starting the batch.
pub(crate) static ENCODER_DISPATCH: StageTiming = StageTiming::new();
pub(crate) static ENCODE: StageTiming = StageTiming::new();
/// Encoding a batch of movement flushes and encoded packets on the writer task.
pub(crate) static INLINE_ENCODE: StageTiming = StageTiming::new();
pub(crate) static SOCKET_WRITE: StageTiming = StageTiming::new();
/// Cache reads that found a Shard's snapshot locked by its pump, and how long they waited.
pub(crate) static SNAPSHOT_WAIT: StageTiming = StageTiming::new();

pub(crate) fn log_line() -> String {
    format!(
        "STAGESTAT {} {} {} {} {} {} {}",
        QUEUE_WAIT.take().field("queue_wait"),
        ENCODER_PERMIT_WAIT.take().field("encoder_permit_wait"),
        ENCODER_DISPATCH.take().field("encoder_dispatch"),
        ENCODE.take().field("encode"),
        INLINE_ENCODE.take().field("inline_encode"),
        SOCKET_WRITE.take().field("write"),
        SNAPSHOT_WAIT.take().field("snapshot_wait"),
    )
}

/// Timing for one SDK pump or fan-out role, such as `shard lyracore-world-1 coordinator`.
pub(crate) struct PumpTiming {
    role: String,
    /// One unit of work: an SDK message with its snapshot lock wait, cache apply and Relay row
    /// callbacks, or one movement fan-out pass.
    message: StageTiming,
    /// The after-message hook, which hands staged movement to the fan-out thread.
    hook: StageTiming,
    /// From the start of one unit of work to the start of the next.
    spacing: StageTiming,
    last_start: Mutex<Option<Instant>>,
}

impl PumpTiming {
    pub(crate) fn record_message(&self, started: Instant) {
        self.message.record(started.elapsed());
        let previous = self.last_start.lock().unwrap().replace(started);
        if let Some(previous) = previous {
            self.spacing.record(started - previous);
        }
    }

    pub(crate) fn record_hook(&self, elapsed: Duration) {
        self.hook.record(elapsed);
    }

    fn take_line(&self, window: Duration) -> Option<String> {
        let (message, hook, spacing) = (self.message.take(), self.hook.take(), self.spacing.take());
        if message.count == 0 && hook.count == 0 {
            return None;
        }
        let busy = (message.total_micros + hook.total_micros) as f64 * 100.0
            / window.as_micros().max(1) as f64;
        Some(format!(
            "PUMPSTAT role=\"{}\" busy={busy:.1}% {} {} {}",
            self.role,
            message.field("message"),
            hook.field("hook"),
            spacing.field("spacing"),
        ))
    }
}

static PUMPS: Mutex<Vec<Arc<PumpTiming>>> = Mutex::new(Vec::new());

/// Returns the timing for `role`; a reconnected pump with the same role shares it.
pub(crate) fn pump_timing(role: &str) -> Arc<PumpTiming> {
    let mut pumps = PUMPS.lock().unwrap();
    if let Some(pump) = pumps.iter().find(|pump| pump.role == role) {
        return pump.clone();
    }
    let pump = Arc::new(PumpTiming {
        role: role.to_owned(),
        message: StageTiming::new(),
        hook: StageTiming::new(),
        spacing: StageTiming::new(),
        last_start: Mutex::new(None),
    });
    pumps.push(pump.clone());
    pump
}

pub(crate) fn pump_lines(window: Duration) -> Vec<String> {
    let pumps = PUMPS.lock().unwrap().clone();
    pumps
        .iter()
        .filter_map(|pump| pump.take_line(window))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_reports_mean_and_max_then_starts_empty() {
        let stage = StageTiming::new();
        stage.record(Duration::from_micros(100));
        stage.record(Duration::from_micros(300));
        assert_eq!(stage.take().field("encode"), "encode=200/300us(2)");
        assert_eq!(stage.take().field("encode"), "encode=0/0us(0)");
    }

    #[test]
    fn a_pump_reports_busy_share_and_message_spacing() {
        let pump = pump_timing("test pump");
        let start = Instant::now();
        pump.record_message(start);
        pump.record_message(start + Duration::from_millis(40));
        pump.record_hook(Duration::from_millis(1));
        let line = pump.take_line(Duration::from_secs(10)).unwrap();
        assert!(line.starts_with("PUMPSTAT role=\"test pump\" busy="));
        assert!(line.contains("hook=1000/1000us(1)"));
        assert!(line.contains("spacing=40000/40000us(1)"));
        assert!(pump.take_line(Duration::from_secs(10)).is_none());
        assert!(Arc::ptr_eq(&pump, &pump_timing("test pump")));
    }
}
