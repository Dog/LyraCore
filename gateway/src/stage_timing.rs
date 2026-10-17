use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

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

    /// Returns `name=<mean>/<max>us(<count>)` for the window since the last call, then resets it.
    fn take(&self, name: &str) -> String {
        let count = self.count.swap(0, Ordering::Relaxed);
        let total = self.total_micros.swap(0, Ordering::Relaxed);
        let max = self.max_micros.swap(0, Ordering::Relaxed);
        format!(
            "{name}={}/{max}us({count})",
            total.checked_div(count).unwrap_or(0)
        )
    }
}

/// One SDK message on any pump: cache lock wait, cache apply and every Relay row callback.
pub(crate) static SDK_MESSAGE: StageTiming = StageTiming::new();
/// Age of the oldest World Session queue entry when its encoding batch starts.
pub(crate) static QUEUE_WAIT: StageTiming = StageTiming::new();
pub(crate) static ENCODER_WAIT: StageTiming = StageTiming::new();
pub(crate) static ENCODE: StageTiming = StageTiming::new();
pub(crate) static SOCKET_WRITE: StageTiming = StageTiming::new();

pub(crate) fn log_line() -> String {
    format!(
        "STAGESTAT {} {} {} {} {}",
        SDK_MESSAGE.take("sdk_message"),
        QUEUE_WAIT.take("queue_wait"),
        ENCODER_WAIT.take("encoder_wait"),
        ENCODE.take("encode"),
        SOCKET_WRITE.take("write"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_reports_mean_and_max_then_starts_empty() {
        let stage = StageTiming::new();
        stage.record(Duration::from_micros(100));
        stage.record(Duration::from_micros(300));
        assert_eq!(stage.take("encode"), "encode=200/300us(2)");
        assert_eq!(stage.take("encode"), "encode=0/0us(0)");
    }
}
