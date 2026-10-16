use super::super::*;
use crate::stdb::ReducerCallError;

pub(crate) type MoveRecord = (u32, f32, f32, f32, f32, u32);

#[derive(Default)]
pub(crate) struct SessionState {
    pub(crate) movement_world:
        Option<std::sync::Arc<crate::world::tests::benilla_tests::MovementWorld>>,
    pub(crate) entity_in_world: bool,
    pub(crate) entity_presence: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    pub(crate) entity_presence_checks: std::sync::atomic::AtomicUsize,
    pub(crate) username: String,
    pub(crate) session: Option<WorldSession>,
    pub(crate) login_entity: Option<codec::EntityView>,
    pub(crate) login_pause: Option<std::sync::Arc<(std::sync::Barrier, std::sync::Barrier)>>,
    pub(crate) moves: std::sync::Mutex<Vec<MoveRecord>>,
    pub(crate) move_bodies: std::sync::Mutex<Vec<Vec<u8>>>,
    pub(crate) client_commands: std::sync::Mutex<Vec<(u64, String, String)>>,
    pub(crate) client_command_error: Option<fn(&str) -> ReducerCallError>,
    pub(crate) turn_in_reward_item: Option<codec::ItemInstanceView>,
    pub(crate) turn_in_tx: std::sync::Mutex<Option<SessionTx>>,
    pub(crate) combat_until_ms: u64,
    pub(crate) logout_called: std::sync::atomic::AtomicBool,
    pub(crate) logout_transport_lost: bool,
    pub(crate) login_entries: std::sync::Mutex<Vec<codec::WorldEntry>>,
    pub(crate) pending_system_messages: Vec<String>,
    pub(crate) login_calls: std::sync::atomic::AtomicU32,
    pub(crate) worldport_entity: Option<codec::EntityView>,
    pub(crate) worldport_login_error: Option<String>,
    pub(crate) subscribed: std::sync::Mutex<Vec<(u64, u32, f32, f32)>>,
    pub(crate) session_depth:
        std::sync::Mutex<Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>>,
    pub(crate) live_guids: Vec<u64>,
    pub(crate) movement_transport_lost: bool,
    pub(crate) relay_view: Option<std::sync::Arc<crate::stdb::world_view::WorldView>>,
    pub(crate) member_stats_before_party_frame: Option<u64>,
}

impl SessionStore for WorldFake {
    fn pending_system_messages(&self, _self_guid: u64) -> Vec<String> {
        self.session.pending_system_messages.clone()
    }

    fn lookup_session(&self, account_name: &str) -> Result<Option<WorldSession>> {
        Ok((account_name == self.session.username)
            .then(|| self.session.session.clone())
            .flatten())
    }

    fn player_login(
        &self,
        _account_id: u64,
        _character: Actor,
        entry: codec::WorldEntry,
    ) -> Result<codec::EntityView> {
        self.rec("player_login");
        if let Some(pause) = &self.session.login_pause {
            pause.0.wait();
            pause.1.wait();
        }
        self.session.login_entries.lock().unwrap().push(entry);
        let call = self
            .session
            .login_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call > 0 {
            if let Some(e) = &self.session.worldport_login_error {
                return Err(anyhow!("{e}"));
            }
            if let Some(e) = self.session.worldport_entity.clone() {
                return Ok(e);
            }
        }
        self.session
            .login_entity
            .clone()
            .ok_or_else(|| anyhow!("no login entity configured"))
    }

    fn movement_update(&self, actor: Actor, opcode: u32, info: &MovementInfo) -> Result<()> {
        self.rec("movement_update");
        if self.session.movement_transport_lost {
            return Err(ReducerCallError::transport_lost("gw_movement_batch").into());
        }
        self.session.moves.lock().unwrap().push((
            opcode,
            info.position.x,
            info.position.y,
            info.position.z,
            info.orientation,
            info.timestamp,
        ));
        self.session
            .move_bodies
            .lock()
            .unwrap()
            .push(codec::movement_info_to_bytes(info)?);
        if let Some(world) = &self.session.movement_world {
            world.update(actor.guid(), opcode, info)?;
        }
        Ok(())
    }

    fn subscribe_player_events(
        &self,
        _account_id: u64,
        self_guid: u64,
        arrival: &codec::EntityView,
        tx: SessionTx,
    ) -> Result<PlayerSubscriptions> {
        self.rec("subscribe_player_events");
        self.session.subscribed.lock().unwrap().push((
            self_guid,
            arrival.map_id,
            arrival.x,
            arrival.y,
        ));
        *self.session.session_depth.lock().unwrap() = Some(tx.depth_handle());
        if self.session.turn_in_reward_item.is_some() {
            *self.session.turn_in_tx.lock().unwrap() = Some(tx.clone());
        }
        let Some(view) = &self.session.relay_view else {
            return Ok(PlayerSubscriptions::empty());
        };
        let subs = PlayerSubscriptions::registered_for_test(view.clone(), self_guid, arrival, tx);
        if let (Some(mate), Some(record)) = (
            self.session.member_stats_before_party_frame,
            subs.member_stats_record(),
        ) {
            let delivered = codec::MemberStats::default();
            record
                .lock()
                .insert(mate, MemberSnapshot::Live(Box::new(delivered)));
        }
        Ok(subs)
    }

    fn client_command(&self, actor: Actor, cmd: String, payload: String) -> Result<()> {
        if let Some(error) = self.session.client_command_error {
            return Err(error("gw_client_command").into());
        }
        self.session
            .client_commands
            .lock()
            .unwrap()
            .push((actor.guid(), cmd, payload));
        Ok(())
    }

    fn entity_in_world(&self, guid: u64) -> bool {
        self.session
            .entity_presence_checks
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(present) = &self.session.entity_presence {
            return present.load(std::sync::atomic::Ordering::SeqCst);
        }
        self.session.entity_in_world || self.session.live_guids.contains(&guid)
    }

    fn entity_max_health(&self, _guid: u64) -> u32 {
        100
    }

    fn claim_session(&self, account_id: u64, _character_guid: u64) -> Result<WorldSessionToken> {
        Ok(WorldSessionToken {
            account_id,
            generation: 1,
            request_nonce: 1,
        })
    }

    fn bind_session(
        &self,
        _token: WorldSessionToken,
    ) -> Result<Option<std::sync::Arc<dyn WorldStore>>> {
        Ok(None)
    }

    fn release_session(&self, _token: WorldSessionToken) -> Result<()> {
        self.rec("logout");
        self.session
            .logout_called
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if self.session.logout_transport_lost {
            return Err(ReducerCallError::transport_lost("logout").into());
        }
        Ok(())
    }

    fn player_combat_until_ms(&self, _player_guid: u64) -> u64 {
        self.session.combat_until_ms
    }
}
