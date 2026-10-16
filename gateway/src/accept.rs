//! Before changing listener retries or capacity limits, read docs/accept-policy.md.

use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Debug, Clone)]
pub(crate) struct BlockingTaskCapacity {
    permits: Arc<Semaphore>,
}

impl BlockingTaskCapacity {
    pub(crate) fn new(limit: usize) -> Self {
        assert!(limit > 0, "blocking-task capacity must be nonzero");
        Self {
            permits: Arc::new(Semaphore::new(limit)),
        }
    }

    pub(crate) fn try_admit(&self) -> Option<BlockingTaskPermit> {
        self.permits
            .clone()
            .try_acquire_owned()
            .ok()
            .map(|permit| BlockingTaskPermit { _permit: permit })
    }

    pub(crate) async fn run<F, T>(&self, work: F) -> anyhow::Result<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let permit = self.permits.clone().acquire_owned().await?;
        Ok(tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work()
        })
        .await?)
    }

    #[cfg(test)]
    pub(crate) fn available(&self) -> usize {
        self.permits.available_permits()
    }
}

pub(crate) struct BlockingTaskPermit {
    _permit: OwnedSemaphorePermit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptOutcome {
    Retry,
    Fatal,
}

const FATAL_ACCEPT_ERRNOS: &[i32] = &[libc::EBADF, libc::ENOTSOCK, libc::EINVAL, libc::EFAULT];

pub fn classify_accept_error(e: &io::Error) -> AcceptOutcome {
    match e.raw_os_error() {
        Some(errno) if FATAL_ACCEPT_ERRNOS.contains(&errno) => AcceptOutcome::Fatal,
        _ => AcceptOutcome::Retry,
    }
}

const BACKOFF_BASE_MS: u64 = 10;
const BACKOFF_CAP_MS: u64 = 1_000;

/// EMFILE returns immediately, so repeated accept failures need backoff to avoid busy-spinning.
pub fn backoff_delay(consecutive: u32) -> Duration {
    if consecutive <= 1 {
        return Duration::ZERO;
    }
    // checked_shl checks the shift count, not lost high bits; clamp before multiplying.
    let steps = (consecutive - 2).min(u64::BITS - 1);
    let ms = BACKOFF_BASE_MS
        .saturating_mul(1u64 << steps)
        .min(BACKOFF_CAP_MS);
    Duration::from_millis(ms)
}

#[derive(Debug, Default, Clone, Copy)]
pub struct AcceptBackoff {
    consecutive: u32,
}

impl AcceptBackoff {
    pub const fn new() -> Self {
        Self { consecutive: 0 }
    }

    pub fn record_success(&mut self) {
        self.consecutive = 0;
    }

    pub fn record_failure(&mut self) -> Duration {
        self.consecutive = self.consecutive.saturating_add(1);
        backoff_delay(self.consecutive)
    }

    pub fn consecutive(&self) -> u32 {
        self.consecutive
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(errno: i32) -> io::Error {
        io::Error::from_raw_os_error(errno)
    }

    #[test]
    fn the_errnos_that_killed_the_realm_are_transient() {
        for (errno, name) in [
            (libc::EMFILE, "EMFILE: this process is out of fds"),
            (libc::ENFILE, "ENFILE — the system is out of fds"),
            (
                libc::ECONNABORTED,
                "ECONNABORTED — peer reset while queued in the backlog",
            ),
            (libc::EINTR, "EINTR — a signal interrupted the call"),
            (libc::ENOBUFS, "ENOBUFS — kernel buffer pressure"),
            (libc::ENOMEM, "ENOMEM — kernel memory pressure"),
        ] {
            assert_eq!(
                classify_accept_error(&err(errno)),
                AcceptOutcome::Retry,
                "{name} must not end the listener"
            );
        }
    }

    #[test]
    fn pending_network_errors_and_per_connection_verdicts_are_transient() {
        for errno in [
            libc::ENETDOWN,
            libc::EPROTO,
            libc::ENOPROTOOPT,
            libc::EHOSTDOWN,
            libc::EHOSTUNREACH,
            libc::EOPNOTSUPP,
            libc::ENETUNREACH,
            libc::ECONNRESET,
            libc::EPERM,
            libc::EAGAIN,
        ] {
            assert_eq!(
                classify_accept_error(&err(errno)),
                AcceptOutcome::Retry,
                "errno {errno} should retry"
            );
        }
    }

    #[test]
    fn only_a_broken_listener_is_fatal() {
        for errno in [libc::EBADF, libc::ENOTSOCK, libc::EINVAL, libc::EFAULT] {
            assert_eq!(
                classify_accept_error(&err(errno)),
                AcceptOutcome::Fatal,
                "errno {errno} means the listener itself is unusable"
            );
        }
        assert_eq!(
            FATAL_ACCEPT_ERRNOS.len(),
            4,
            "the fatal set is these four and no others"
        );
    }

    #[test]
    fn an_unknown_errno_retries_rather_than_ending_the_realm() {
        assert_eq!(classify_accept_error(&err(4095)), AcceptOutcome::Retry);
    }

    #[test]
    fn an_error_with_no_raw_errno_retries() {
        let synthetic = io::Error::other("no errno here");
        assert_eq!(synthetic.raw_os_error(), None);
        assert_eq!(classify_accept_error(&synthetic), AcceptOutcome::Retry);
    }

    #[test]
    fn the_first_failure_retries_immediately_then_backs_off() {
        assert_eq!(backoff_delay(0), Duration::ZERO);
        assert_eq!(backoff_delay(1), Duration::ZERO);
        assert_eq!(backoff_delay(2), Duration::from_millis(10));
        assert_eq!(backoff_delay(3), Duration::from_millis(20));
        assert_eq!(backoff_delay(4), Duration::from_millis(40));
        assert_eq!(backoff_delay(8), Duration::from_millis(640));
    }

    #[test]
    fn the_backoff_saturates_at_one_second_and_never_wraps() {
        assert_eq!(backoff_delay(9), Duration::from_millis(1_000));
        for n in [10u32, 64, 65, 1_000, u32::MAX] {
            assert_eq!(
                backoff_delay(n),
                Duration::from_millis(BACKOFF_CAP_MS),
                "consecutive={n} must stay at the cap"
            );
        }
    }

    #[test]
    fn an_accepted_connection_clears_the_backoff() {
        let mut b = AcceptBackoff::new();
        assert_eq!(b.record_failure(), Duration::ZERO);
        assert_eq!(b.record_failure(), Duration::from_millis(10));
        assert_eq!(b.record_failure(), Duration::from_millis(20));
        assert_eq!(b.consecutive(), 3);
        b.record_success();
        assert_eq!(b.consecutive(), 0);
        assert_eq!(b.record_failure(), Duration::ZERO);
    }

    #[test]
    fn the_counter_saturates_rather_than_wrapping() {
        let mut b = AcceptBackoff {
            consecutive: u32::MAX,
        };
        assert_eq!(b.record_failure(), Duration::from_millis(BACKOFF_CAP_MS));
        assert_eq!(b.consecutive(), u32::MAX);
    }

    #[test]
    fn excess_connections_are_refused_without_waiting_for_a_permit() {
        let capacity = BlockingTaskCapacity::new(2);
        let clone = capacity.clone();
        let _first = capacity.try_admit().expect("the first task has a seat");
        let _second = clone.try_admit().expect("the second task has a seat");

        assert!(
            capacity.try_admit().is_none(),
            "an excess connection must not enter a wait queue"
        );
        assert_eq!(capacity.available(), 0);

        drop(_first);
        let _replacement = capacity
            .try_admit()
            .expect("a task that exits before submission returns its seat");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn blocking_task_capacity_returns_after_errors_and_panics() {
        let capacity = BlockingTaskCapacity::new(1);

        let permit = capacity.try_admit().expect("the failing task has a seat");
        let failed = tokio::task::spawn_blocking(move || {
            let _task_permit = permit;
            Err::<(), ()>(())
        })
        .await
        .expect("the failing task returns normally");
        assert!(failed.is_err());
        assert_eq!(capacity.available(), 1, "an error must return its seat");

        let permit = capacity.try_admit().expect("the panicking task has a seat");
        let panicked = tokio::task::spawn_blocking(move || {
            let _task_permit = permit;
            panic!("test panic");
        })
        .await;
        assert!(panicked.unwrap_err().is_panic());
        assert_eq!(capacity.available(), 1, "an unwind must return its seat");
    }
}
