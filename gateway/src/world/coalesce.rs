use std::time::Instant;
use wow_world_messages::vanilla::{MovementInfo, MovementInfo_MovementFlags};

pub const MOVE_COALESCE_WINDOW_MS: u64 = 150;

pub struct CoalesceState {
    epoch: Instant,
    last_forwarded: Option<(MovementInfo_MovementFlags, f32, u64)>,
    pending: Option<(u32, MovementInfo)>,
    received: u64,
    forwarded: u64,
}

impl Default for CoalesceState {
    fn default() -> Self {
        Self {
            epoch: Instant::now(),
            last_forwarded: None,
            pending: None,
            received: 0,
            forwarded: 0,
        }
    }
}

impl CoalesceState {
    pub fn received(&self) -> u64 {
        self.received
    }

    pub fn forwarded(&self) -> u64 {
        self.forwarded
    }

    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    pub fn on_movement_now(
        &mut self,
        opcode: u32,
        info: &MovementInfo,
    ) -> Vec<(u32, MovementInfo)> {
        let now = self.now_ms();
        self.on_movement(opcode, info, now)
    }

    pub fn flush_now(&mut self) -> Option<(u32, MovementInfo)> {
        let now = self.now_ms();
        self.flush(now)
    }

    fn on_movement(
        &mut self,
        opcode: u32,
        info: &MovementInfo,
        now_ms: u64,
    ) -> Vec<(u32, MovementInfo)> {
        self.received += 1;
        let is_heartbeat = opcode == lyracore_shared::opcodes::movement::MSG_MOVE_HEARTBEAT;
        let same_as_last_forwarded = self
            .last_forwarded
            .as_ref()
            .is_some_and(|(f, h, _)| *f == info.flags && *h == info.orientation);
        let is_state_change = !is_heartbeat || !same_as_last_forwarded;

        if is_state_change {
            let mut out = Vec::with_capacity(2);
            // Flush before turn or stop to prevent peers extrapolating beyond the last position.
            if let Some(flushed) = self.flush(now_ms) {
                out.push(flushed);
            }
            self.mark_forwarded(info, now_ms);
            out.push((opcode, info.clone()));
            return out;
        }

        let elapsed_ok = self
            .last_forwarded
            .as_ref()
            .map(|(_, _, t)| now_ms.saturating_sub(*t) >= MOVE_COALESCE_WINDOW_MS)
            .unwrap_or(true);
        if elapsed_ok {
            self.mark_forwarded(info, now_ms);
            vec![(opcode, info.clone())]
        } else {
            self.pending = Some((opcode, info.clone()));
            Vec::new()
        }
    }

    fn flush(&mut self, now_ms: u64) -> Option<(u32, MovementInfo)> {
        let (opcode, info) = self.pending.take()?;
        self.mark_forwarded(&info, now_ms);
        Some((opcode, info))
    }

    fn mark_forwarded(&mut self, info: &MovementInfo, now_ms: u64) {
        self.last_forwarded = Some((info.flags.clone(), info.orientation, now_ms));
        self.forwarded += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lyracore_shared::opcodes::movement as m;
    use wow_world_messages::vanilla::Vector3d;

    fn info(flags: MovementInfo_MovementFlags, orientation: f32, x: f32) -> MovementInfo {
        MovementInfo {
            flags,
            timestamp: 0,
            position: Vector3d { x, y: 0.0, z: 0.0 },
            orientation,
            fall_time: 0.0,
        }
    }

    fn empty() -> MovementInfo_MovementFlags {
        MovementInfo_MovementFlags::empty()
    }

    #[test]
    fn first_packet_ever_forwards_immediately_even_if_its_a_heartbeat() {
        let mut cs = CoalesceState::default();
        let out = cs.on_movement(m::MSG_MOVE_HEARTBEAT, &info(empty(), 0.0, 0.0), 0);
        assert_eq!(
            out.len(),
            1,
            "the very first movement packet must never be held pending"
        );
        assert_eq!(cs.forwarded(), 1);
    }

    #[test]
    fn non_heartbeat_opcodes_always_forward_immediately_regardless_of_flags() {
        let mut cs = CoalesceState::default();
        cs.on_movement(m::MSG_MOVE_HEARTBEAT, &info(empty(), 0.0, 0.0), 0);
        for (i, opcode) in [
            m::MSG_MOVE_START_FORWARD,
            m::MSG_MOVE_STOP,
            m::MSG_MOVE_START_TURN_LEFT,
            m::MSG_MOVE_STOP_TURN,
            m::MSG_MOVE_JUMP,
            m::MSG_MOVE_FALL_LAND,
            m::MSG_MOVE_START_STRAFE_LEFT,
            m::MSG_MOVE_STOP_STRAFE,
            m::MSG_MOVE_SET_FACING,
        ]
        .into_iter()
        .enumerate()
        {
            let out = cs.on_movement(opcode, &info(empty(), 0.0, 0.0), (i as u64) * 10);
            assert_eq!(
                out.len(),
                1,
                "opcode 0x{opcode:03X} must forward immediately"
            );
            assert_eq!(out[0].0, opcode);
        }
    }

    #[test]
    fn state_change_flushes_pending_heartbeat_first_then_itself_in_order() {
        let mut cs = CoalesceState::default();
        let run_flags = MovementInfo_MovementFlags::new(0x1, None, None, None, None);
        assert_eq!(
            cs.on_movement(
                m::MSG_MOVE_START_FORWARD,
                &info(run_flags.clone(), 0.0, 0.0),
                0
            )
            .len(),
            1
        );

        let held = info(run_flags.clone(), 0.0, 1.0);
        let out = cs.on_movement(m::MSG_MOVE_HEARTBEAT, &held, 10);
        assert!(
            out.is_empty(),
            "same-vector heartbeat well inside the window must be held, not forwarded"
        );

        let turn_flags = MovementInfo_MovementFlags::new(0x1 | 0x4, None, None, None, None);
        let turn_info = info(turn_flags, 5.0, 1.0);
        let out = cs.on_movement(m::MSG_MOVE_START_TURN_LEFT, &turn_info, 20);
        assert_eq!(out.len(), 2, "expected [flushed heartbeat, turn]");
        assert_eq!(
            out[0].0,
            m::MSG_MOVE_HEARTBEAT,
            "the held heartbeat must flush FIRST"
        );
        assert_eq!(out[0].1.position.x, 1.0);
        assert_eq!(
            out[1].0,
            m::MSG_MOVE_START_TURN_LEFT,
            "then the turn itself, undelayed"
        );
        assert_eq!(out[1].1.orientation, 5.0);
    }

    #[test]
    fn heartbeat_with_drifted_heading_is_treated_as_a_state_change() {
        let mut cs = CoalesceState::default();
        cs.on_movement(m::MSG_MOVE_START_FORWARD, &info(empty(), 0.0, 0.0), 0);
        let drifted = info(empty(), 1.5, 1.0);
        let out = cs.on_movement(m::MSG_MOVE_HEARTBEAT, &drifted, 5);
        assert_eq!(
            out.len(),
            1,
            "a heading-drifted heartbeat must forward immediately, not coalesce"
        );
    }

    #[test]
    fn heartbeat_with_drifted_flags_is_treated_as_a_state_change() {
        let mut cs = CoalesceState::default();
        cs.on_movement(m::MSG_MOVE_START_FORWARD, &info(empty(), 0.0, 0.0), 0);
        let swim_flags = MovementInfo_MovementFlags::new(0x0020_0000, None, None, None, None);
        let drifted = info(swim_flags, 0.0, 1.0);
        let out = cs.on_movement(m::MSG_MOVE_HEARTBEAT, &drifted, 5);
        assert_eq!(
            out.len(),
            1,
            "a flags-drifted heartbeat must forward immediately, not coalesce"
        );
    }

    #[test]
    fn same_vector_heartbeat_forwards_once_the_window_elapses() {
        let mut cs = CoalesceState::default();
        let flags = empty();
        cs.on_movement(m::MSG_MOVE_HEARTBEAT, &info(flags.clone(), 0.0, 0.0), 0);
        let held = cs.on_movement(m::MSG_MOVE_HEARTBEAT, &info(flags.clone(), 0.0, 1.0), 100);
        assert!(held.is_empty(), "100ms < 150ms window -> held");
        let forwarded = cs.on_movement(m::MSG_MOVE_HEARTBEAT, &info(flags, 0.0, 2.0), 150);
        assert_eq!(forwarded.len(), 1, "150ms >= window -> forwards");
        assert_eq!(
            forwarded[0].1.position.x, 2.0,
            "forwards the LATEST pending position, not the superseded one"
        );
    }

    #[test]
    fn flush_now_style_flush_drains_pending_and_marks_it_forwarded() {
        let mut cs = CoalesceState::default();
        let flags = empty();
        cs.on_movement(m::MSG_MOVE_HEARTBEAT, &info(flags.clone(), 0.0, 0.0), 0);
        cs.on_movement(m::MSG_MOVE_HEARTBEAT, &info(flags, 0.0, 3.0), 50);
        assert_eq!(
            cs.forwarded(),
            1,
            "only the baseline packet has forwarded so far"
        );
        let flushed = cs.flush(60);
        assert_eq!(flushed.unwrap().1.position.x, 3.0);
        assert_eq!(cs.forwarded(), 2, "the flush itself counts as a forward");
        assert!(
            cs.flush(70).is_none(),
            "nothing left to flush a second time"
        );
    }

    #[test]
    fn intermediate_held_heartbeats_are_superseded_not_queued() {
        let mut cs = CoalesceState::default();
        let flags = empty();
        cs.on_movement(m::MSG_MOVE_HEARTBEAT, &info(flags.clone(), 0.0, 0.0), 0);
        cs.on_movement(m::MSG_MOVE_HEARTBEAT, &info(flags.clone(), 0.0, 1.0), 30);
        cs.on_movement(m::MSG_MOVE_HEARTBEAT, &info(flags.clone(), 0.0, 2.0), 60);
        cs.on_movement(m::MSG_MOVE_HEARTBEAT, &info(flags, 0.0, 3.0), 90);
        let flushed = cs.flush(95).expect("a pending heartbeat is waiting");
        assert_eq!(
            flushed.1.position.x, 3.0,
            "only the newest superseding value ships — 1.0 and 2.0 dropped entirely"
        );
    }

    #[test]
    fn synthetic_60ms_heartbeat_stream_forwards_roughly_a_third() {
        let mut cs = CoalesceState::default();
        let flags = empty();
        let n = 60u64;
        let mut forwarded_count = 0u64;
        for i in 0..n {
            let t = i * 60;
            let out = cs.on_movement(
                m::MSG_MOVE_HEARTBEAT,
                &info(flags.clone(), 0.0, i as f32),
                t,
            );
            forwarded_count += out.len() as u64;
        }
        assert_eq!(forwarded_count, cs.forwarded());
        let ratio = n as f64 / forwarded_count as f64;
        assert!(
            (2.5..=3.5).contains(&ratio),
            "expected ~3x reduction (N/forwarded in [2.5, 3.5]), got N={n} forwarded={forwarded_count} ratio={ratio:.2}"
        );
    }

    #[test]
    fn stationary_after_running_leaves_one_heartbeat_pending_until_next_packet() {
        let mut cs = CoalesceState::default();
        let flags = empty();
        cs.on_movement(m::MSG_MOVE_HEARTBEAT, &info(flags.clone(), 0.0, 0.0), 0);
        let held = cs.on_movement(m::MSG_MOVE_HEARTBEAT, &info(flags, 0.0, 9.0), 40);
        assert!(
            held.is_empty(),
            "the player's last heartbeat before going idle sits pending"
        );
        let flushed = cs.flush(5_000);
        assert_eq!(
            flushed.unwrap().1.position.x,
            9.0,
            "the held position survives, unmodified, until flushed"
        );
    }
}
