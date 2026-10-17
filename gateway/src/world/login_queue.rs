use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const ADMIT_TICK: Duration = Duration::from_secs(1);
/// An admitted session that never finishes entering the world frees its entering slot after this.
const ENTERING_TIMEOUT: Duration = Duration::from_secs(20);

pub type Ticket = u64;

#[derive(Debug, PartialEq, Eq)]
pub enum Admission {
    Admitted(Ticket),
    Queued(Ticket),
}

struct Waiter {
    ticket: Ticket,
    queued_at: Instant,
}

struct State {
    active: usize,
    waiting: VecDeque<Waiter>,
    entering: HashMap<Ticket, Instant>,
    next_ticket: Ticket,
    tick_started: Instant,
    admitted_this_tick: usize,
}

pub struct LoginQueue {
    max_sessions: usize,
    admit_concurrency: usize,
    max_entering: usize,
    tick: Duration,
    state: Mutex<State>,
    admitted_total: AtomicU64,
}

impl LoginQueue {
    #[cfg(test)]
    pub fn new(max_sessions: usize, admit_concurrency: usize) -> Self {
        Self::with_tick(max_sessions, admit_concurrency, ADMIT_TICK)
    }

    #[cfg(test)]
    pub fn with_tick(max_sessions: usize, admit_concurrency: usize, tick: Duration) -> Self {
        Self::with_entry_gate(max_sessions, admit_concurrency, 0, tick)
    }

    pub fn with_entry_gate(
        max_sessions: usize,
        admit_concurrency: usize,
        max_entering: usize,
        tick: Duration,
    ) -> Self {
        Self {
            max_sessions,
            admit_concurrency,
            max_entering,
            tick,
            state: Mutex::new(State {
                active: 0,
                waiting: VecDeque::new(),
                entering: HashMap::new(),
                next_ticket: 0,
                tick_started: Instant::now(),
                admitted_this_tick: 0,
            }),
            admitted_total: AtomicU64::new(0),
        }
    }

    #[cfg(test)]
    pub fn unlimited() -> Self {
        Self::new(0, 0)
    }

    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self::with_entry_gate(
            crate::config::max_sessions()?,
            crate::config::admit_concurrency()?,
            crate::config::max_entering()?,
            ADMIT_TICK,
        ))
    }

    fn gated(&self) -> bool {
        self.max_sessions > 0 || self.max_entering > 0
    }

    fn has_room(&self, s: &State) -> bool {
        (self.max_sessions == 0 || s.active < self.max_sessions)
            && (self.max_entering == 0 || s.entering.len() < self.max_entering)
    }

    pub fn request(&self) -> Admission {
        let mut s = self.state.lock().unwrap();
        let ticket = s.next_ticket;
        s.next_ticket += 1;
        if !self.gated() {
            return Admission::Admitted(ticket);
        }
        self.expire_entering(&mut s);
        if s.waiting.is_empty() && self.has_room(&s) {
            s.active += 1;
            if self.max_entering > 0 {
                s.entering.insert(ticket, Instant::now());
            }
            self.admitted_total.fetch_add(1, Ordering::Relaxed);
            return Admission::Admitted(ticket);
        }
        s.waiting.push_back(Waiter {
            ticket,
            queued_at: Instant::now(),
        });
        Admission::Queued(ticket)
    }

    pub fn position(&self, ticket: Ticket) -> Option<usize> {
        let mut s = self.state.lock().unwrap();
        self.drain_locked(&mut s);
        s.waiting
            .iter()
            .position(|w| w.ticket == ticket)
            .map(|i| i + 1)
    }

    #[cfg(test)]
    pub fn cancel(&self, ticket: Ticket) {
        let mut s = self.state.lock().unwrap();
        s.waiting.retain(|w| w.ticket != ticket);
    }

    pub(super) fn cancel_or_depart(&self, ticket: Ticket) {
        let mut state = self.state.lock().unwrap();
        if let Some(index) = state
            .waiting
            .iter()
            .position(|waiter| waiter.ticket == ticket)
        {
            state.waiting.remove(index);
        } else {
            state.active = state.active.saturating_sub(1);
            state.entering.remove(&ticket);
            self.drain_locked(&mut state);
        }
    }

    pub fn depart(&self) {
        if !self.gated() {
            return;
        }
        let mut s = self.state.lock().unwrap();
        s.active = s.active.saturating_sub(1);
        self.drain_locked(&mut s);
    }

    /// Frees `ticket`'s entering slot; repeats and unknown tickets are no-ops.
    pub fn finish_entering(&self, ticket: Ticket) {
        let mut s = self.state.lock().unwrap();
        if s.entering.remove(&ticket).is_some() {
            self.drain_locked(&mut s);
        }
    }

    fn expire_entering(&self, s: &mut State) {
        if self.max_entering > 0 {
            s.entering
                .retain(|_, admitted| admitted.elapsed() < ENTERING_TIMEOUT);
        }
    }

    pub fn tick(&self) {
        let mut s = self.state.lock().unwrap();
        self.drain_locked(&mut s);
    }

    fn drain_locked(&self, s: &mut State) {
        if self.admit_concurrency > 0 {
            let now = Instant::now();
            if now.duration_since(s.tick_started) >= self.tick {
                s.tick_started = now;
                s.admitted_this_tick = 0;
            }
        }
        self.expire_entering(s);
        loop {
            if !self.has_room(s) {
                break;
            }
            if self.admit_concurrency > 0 && s.admitted_this_tick >= self.admit_concurrency {
                break;
            }
            let Some(front) = s.waiting.pop_front() else {
                break;
            };
            s.active += 1;
            if self.max_entering > 0 {
                s.entering.insert(front.ticket, Instant::now());
            }
            s.admitted_this_tick += 1;
            self.admitted_total.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn depth(&self) -> usize {
        self.state.lock().unwrap().waiting.len()
    }

    pub fn active(&self) -> usize {
        self.state.lock().unwrap().active
    }

    pub fn entering(&self) -> usize {
        self.state.lock().unwrap().entering.len()
    }

    pub fn oldest_wait(&self) -> Option<Duration> {
        let s = self.state.lock().unwrap();
        s.waiting.front().map(|w| w.queued_at.elapsed())
    }

    pub fn admitted_total(&self) -> u64 {
        self.admitted_total.load(Ordering::Relaxed)
    }

    pub fn max_sessions(&self) -> usize {
        self.max_sessions
    }

    pub fn admit_concurrency(&self) -> usize {
        self.admit_concurrency
    }

    pub fn max_entering(&self) -> usize {
        self.max_entering
    }
}

/// Holds an admitted session's entering slot; dropping it frees the slot for the next waiter.
pub(crate) struct EntrySlot {
    queue: std::sync::Arc<LoginQueue>,
    ticket: Ticket,
}

impl EntrySlot {
    pub(crate) fn new(queue: std::sync::Arc<LoginQueue>, ticket: Ticket) -> Self {
        Self { queue, ticket }
    }
}

impl Drop for EntrySlot {
    fn drop(&mut self) {
        self.queue.finish_entering(self.ticket);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_simultaneous_login_storm_never_seats_more_than_the_cap() {
        const CAP: usize = 4;
        const ARRIVALS: usize = 64;
        let q = std::sync::Arc::new(LoginQueue::new(CAP, 0));

        let admitted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handles: Vec<_> = (0..ARRIVALS)
            .map(|_| {
                let q = q.clone();
                let admitted = admitted.clone();
                std::thread::spawn(move || {
                    if matches!(q.request(), Admission::Admitted(_)) {
                        admitted.fetch_add(1, Ordering::Relaxed);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("no arrival thread may panic");
        }

        let admitted = admitted.load(Ordering::Relaxed);
        assert_eq!(
            admitted, CAP,
            "with {ARRIVALS} simultaneous arrivals and {CAP} seats, exactly {CAP} must be admitted \
            — fewer means seats were stranded, more means the cap was breached under contention"
        );
        assert_eq!(
            q.active(),
            CAP,
            "the ledger must agree with what was handed out"
        );
        assert_eq!(
            q.depth(),
            ARRIVALS - CAP,
            "every arrival that was not seated must be IN LINE, not silently dropped"
        );
        assert_eq!(
            q.admitted_total(),
            CAP as u64,
            "the QUEUESTAT counter must count each admission exactly once"
        );
    }

    #[test]
    fn every_seat_returns_to_the_pool_when_its_session_ends() {
        let q = LoginQueue::new(3, 0);
        for round in 0..2 {
            for _ in 0..3 {
                assert!(
                    matches!(q.request(), Admission::Admitted(_)),
                    "round {round}"
                );
            }
            assert_eq!(q.active(), 3);
            let Admission::Queued(t) = q.request() else {
                panic!("round {round}: the 4th arrival must queue behind the 3 seats");
            };
            q.cancel(t);

            for _ in 0..3 {
                q.depart();
            }
            assert_eq!(
                q.active(),
                0,
                "round {round}: every seat must come back — a leaked seat permanently shrinks the \
                 realm's capacity and only a restart recovers it"
            );
            assert_eq!(q.depth(), 0, "round {round}: nobody may be left in line");
        }
    }

    #[test]
    fn releasing_a_seat_more_often_than_it_was_taken_never_underflows_the_cap() {
        let q = LoginQueue::new(2, 0);
        assert!(matches!(q.request(), Admission::Admitted(_)));
        q.depart();
        q.depart();
        q.depart();
        assert_eq!(
            q.active(),
            0,
            "an extra release must clamp at zero, not wrap to usize::MAX — a wrapped counter makes \
             `active < max_sessions` false forever and every subsequent login queues behind nothing"
        );
        assert!(matches!(q.request(), Admission::Admitted(_)));
        assert!(matches!(q.request(), Admission::Admitted(_)));
        assert!(matches!(q.request(), Admission::Queued(_)));
    }

    #[test]
    fn the_unlimited_queue_keeps_no_ledger_on_departure() {
        let q = LoginQueue::unlimited();
        for _ in 0..100 {
            assert!(matches!(q.request(), Admission::Admitted(_)));
            q.depart();
        }
        assert_eq!(q.active(), 0);
        assert_eq!(q.depth(), 0);
        assert_eq!(
            q.admitted_total(),
            0,
            "the unlimited queue is a pure pass-through — it never even counts"
        );
    }

    #[test]
    fn tick_drains_the_line_into_free_capacity_without_any_other_event() {
        let q = LoginQueue::new(1, 0);
        assert!(matches!(q.request(), Admission::Admitted(_)));
        let Admission::Queued(t) = q.request() else {
            panic!("the second arrival must queue behind the one seat");
        };
        assert_eq!(q.position(t), Some(1));

        q.state.lock().unwrap().active = 0;
        assert_eq!(
            q.depth(),
            1,
            "precondition: the waiter is still in line and nothing has drained it"
        );

        q.tick();
        assert_eq!(
            q.depth(),
            0,
            "tick() must move the waiter into the free seat — without it, a line can sit still \
             while capacity is available"
        );
        assert_eq!(q.active(), 1);
        assert_eq!(
            q.position(t),
            None,
            "an admitted ticket must stop reporting a position, which is how the waiter learns to \
             proceed"
        );
    }

    #[test]
    fn tick_is_inert_when_there_is_nothing_to_drain() {
        let q = LoginQueue::new(2, 0);
        q.tick();
        assert_eq!((q.active(), q.depth(), q.admitted_total()), (0, 0, 0));

        let u = LoginQueue::unlimited();
        u.tick();
        assert_eq!((u.active(), u.depth(), u.admitted_total()), (0, 0, 0));
    }

    #[test]
    fn the_configured_limits_are_reported_back_verbatim() {
        let q = LoginQueue::new(250, 8);
        assert_eq!(q.max_sessions(), 250);
        assert_eq!(q.admit_concurrency(), 8);

        let u = LoginQueue::unlimited();
        assert_eq!(u.max_sessions(), 0, "0 is the unlimited sentinel");
        assert_eq!(u.admit_concurrency(), 0);
    }

    #[test]
    fn a_full_entry_gate_queues_arrivals_until_an_entering_session_finishes() {
        let q = LoginQueue::with_entry_gate(0, 0, 2, ADMIT_TICK);
        let Admission::Admitted(first) = q.request() else {
            panic!("the first arrival enters");
        };
        assert!(matches!(q.request(), Admission::Admitted(_)));
        let Admission::Queued(third) = q.request() else {
            panic!("a third arrival waits while two are entering");
        };
        assert_eq!(q.position(third), Some(1));

        q.finish_entering(first);
        q.finish_entering(first);

        assert_eq!(q.position(third), None, "the waiter is admitted");
        assert_eq!(q.entering(), 2);
        assert_eq!(q.active(), 3, "finishing entry keeps the session seated");
    }

    #[test]
    fn a_promoted_waiter_that_leaves_frees_its_entering_slot() {
        let q = LoginQueue::with_entry_gate(0, 0, 1, ADMIT_TICK);
        let Admission::Admitted(first) = q.request() else {
            panic!("the first arrival enters");
        };
        let Admission::Queued(second) = q.request() else {
            panic!("the second arrival waits");
        };
        q.finish_entering(first);
        assert_eq!(q.position(second), None);

        q.cancel_or_depart(second);

        assert_eq!(q.entering(), 0);
        assert!(matches!(q.request(), Admission::Admitted(_)));
    }

    #[test]
    fn a_cancelled_tickets_number_is_never_handed_out_again() {
        let q = LoginQueue::new(1, 0);
        let Admission::Admitted(seated) = q.request() else {
            panic!("the first arrival takes the seat");
        };

        let mut seen = Vec::new();
        for _ in 0..5 {
            let Admission::Queued(t) = q.request() else {
                panic!("every arrival past the one seat must queue");
            };
            assert!(
                !seen.contains(&t),
                "ticket {t} was issued twice — a stale ticket would then address a live waiter"
            );
            seen.push(t);
            q.cancel(t);
        }
        assert_eq!(
            seen,
            (seated + 1..seated + 6).collect::<Vec<_>>(),
            "tickets must be strictly increasing, never recycled from cancellations"
        );
        assert_eq!(q.depth(), 0, "every one of them left the line");
    }

    #[test]
    fn oldest_wait_is_none_when_nobody_is_in_line() {
        let q = LoginQueue::new(1, 0);
        assert_eq!(q.oldest_wait(), None, "an empty queue has no oldest waiter");
        assert!(matches!(q.request(), Admission::Admitted(_)));
        assert_eq!(
            q.oldest_wait(),
            None,
            "an ADMITTED session is not waiting — only queued arrivals count"
        );
        let Admission::Queued(t) = q.request() else {
            panic!("expected a queued arrival");
        };
        assert!(q.oldest_wait().is_some(), "a queued arrival is a waiter");
        q.cancel(t);
        assert_eq!(
            q.oldest_wait(),
            None,
            "and it stops being one the moment it leaves"
        );
    }

    #[test]
    fn unlimited_never_queues_regardless_of_volume() {
        let q = LoginQueue::unlimited();
        for _ in 0..1000 {
            assert!(matches!(q.request(), Admission::Admitted(_)));
        }
        assert_eq!(q.depth(), 0);
        assert_eq!(q.active(), 0);
    }

    #[test]
    fn cap_zero_is_the_unlimited_shape_too() {
        let q = LoginQueue::new(0, 5);
        assert!(matches!(q.request(), Admission::Admitted(_)));
        assert_eq!(q.depth(), 0);
    }

    #[test]
    fn fifo_order_and_positions() {
        let q = LoginQueue::new(1, 0);
        assert!(matches!(q.request(), Admission::Admitted(_)));

        let Admission::Queued(a) = q.request() else {
            panic!("expected A to queue")
        };
        let Admission::Queued(b) = q.request() else {
            panic!("expected B to queue")
        };
        let Admission::Queued(c) = q.request() else {
            panic!("expected C to queue")
        };

        assert_eq!(q.position(a), Some(1));
        assert_eq!(q.position(b), Some(2));
        assert_eq!(q.position(c), Some(3));
        assert_eq!(q.depth(), 3);
    }

    #[test]
    fn position_updates_on_admission_in_fifo_order() {
        let q = LoginQueue::new(1, 0);
        assert!(matches!(q.request(), Admission::Admitted(_)));

        let Admission::Queued(a) = q.request() else {
            panic!()
        };
        let Admission::Queued(b) = q.request() else {
            panic!()
        };
        let Admission::Queued(c) = q.request() else {
            panic!()
        };
        assert_eq!(
            (q.position(a), q.position(b), q.position(c)),
            (Some(1), Some(2), Some(3))
        );

        q.depart();
        assert_eq!(
            q.position(a),
            None,
            "A should be admitted — it was at the front"
        );
        assert_eq!(q.position(b), Some(1), "B moves up to the front");
        assert_eq!(q.position(c), Some(2));

        q.depart();
        assert_eq!(q.position(b), None);
        assert_eq!(q.position(c), Some(1));

        q.depart();
        assert_eq!(q.position(c), None);
        assert_eq!(q.depth(), 0);
    }

    #[test]
    fn disconnect_while_queued_just_leaves_the_line() {
        let q = LoginQueue::new(1, 0);
        assert!(matches!(q.request(), Admission::Admitted(_)));

        let Admission::Queued(a) = q.request() else {
            panic!()
        };
        let Admission::Queued(b) = q.request() else {
            panic!()
        };
        assert_eq!(q.depth(), 2);

        q.cancel(a);
        assert_eq!(q.depth(), 1);
        assert_eq!(
            q.position(a),
            None,
            "cancelled tickets read the same as admitted — gone either way"
        );
        assert_eq!(
            q.position(b),
            Some(1),
            "B moves up even though nobody was admitted"
        );
        assert_eq!(
            q.active(),
            1,
            "cancelling a WAITING ticket must never free or grant a seat"
        );

        q.depart();
        assert_eq!(q.position(b), None);
        assert_eq!(q.active(), 1);
        assert_eq!(q.depth(), 0);
    }

    #[test]
    fn cancelling_an_already_admitted_ticket_is_a_harmless_noop() {
        let q = LoginQueue::new(0, 0);
        q.cancel(999);
        assert_eq!(q.depth(), 0);
    }

    fn fill_to_capacity(q: &LoginQueue, cap: usize) {
        for _ in 0..cap {
            assert!(matches!(q.request(), Admission::Admitted(_)));
        }
        assert_eq!(q.active(), cap);
    }

    #[test]
    fn admit_concurrency_throttles_the_drain_rate_even_with_free_capacity() {
        let q = LoginQueue::with_tick(10, 1, Duration::from_secs(3600));
        fill_to_capacity(&q, 10);
        let Admission::Queued(a) = q.request() else {
            panic!("expected A to queue: the house is full")
        };
        let Admission::Queued(b) = q.request() else {
            panic!()
        };
        let Admission::Queued(c) = q.request() else {
            panic!()
        };
        assert_eq!(q.depth(), 3);

        for _ in 0..5 {
            q.depart();
        }
        assert_eq!(
            q.position(a),
            None,
            "A admitted — the one this tick's allowance covers"
        );
        assert_eq!(
            q.position(b),
            Some(1),
            "B and C stay queued: this tick's allowance is spent"
        );
        assert_eq!(q.position(c), Some(2));
        assert_eq!(
            q.active(),
            6,
            "5 seats freed, only 1 refilled: 10 - 5 + 1 = 6"
        );

        assert_eq!(q.position(b), Some(1));
    }

    #[test]
    fn admit_concurrency_releases_more_on_the_next_tick() {
        let tick = Duration::from_millis(20);
        let q = LoginQueue::with_tick(10, 1, tick);
        fill_to_capacity(&q, 10);
        let Admission::Queued(a) = q.request() else {
            panic!()
        };
        let Admission::Queued(b) = q.request() else {
            panic!()
        };
        q.depart();
        q.depart();
        assert_eq!(q.position(a), None, "A admitted this tick");
        assert_eq!(
            q.position(b),
            Some(1),
            "B waits for the next tick even though a 2nd seat freed"
        );

        std::thread::sleep(tick * 3);
        assert_eq!(q.position(b), None, "the next tick's allowance admits B");
    }

    #[test]
    fn oldest_wait_reports_the_front_of_the_line() {
        let q = LoginQueue::new(1, 0);
        assert!(q.oldest_wait().is_none(), "nobody waiting yet");
        assert!(matches!(q.request(), Admission::Admitted(_)));
        assert!(
            q.oldest_wait().is_none(),
            "the one session is active, not waiting"
        );

        let Admission::Queued(_a) = q.request() else {
            panic!()
        };
        std::thread::sleep(Duration::from_millis(5));
        let waited = q.oldest_wait().expect("A is waiting");
        assert!(waited >= Duration::from_millis(5));
    }

    #[test]
    fn admitted_total_counts_both_immediate_and_drained_admissions() {
        let q = LoginQueue::new(1, 0);
        assert_eq!(q.admitted_total(), 0);
        assert!(matches!(q.request(), Admission::Admitted(_)));
        assert_eq!(q.admitted_total(), 1);

        let Admission::Queued(a) = q.request() else {
            panic!()
        };
        assert_eq!(q.admitted_total(), 1, "queuing does not itself admit");
        q.depart();
        let _ = a;
        assert_eq!(
            q.admitted_total(),
            2,
            "the drain on depart counts as an admission"
        );
    }
}
