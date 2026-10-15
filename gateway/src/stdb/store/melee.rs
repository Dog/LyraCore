use anyhow::Result;

use crate::stdb::bindings::*;
use crate::stdb::connection::call_reducer;
use crate::stdb::Coordinator;
use crate::world::{Actor, MeleeActionStore};

impl MeleeActionStore for Coordinator {
    fn start_attack(&self, actor: Actor, target_guid: u64) -> Result<()> {
        let coord = self.0.call_pipe();
        call_reducer!(
            coord.conn.reducers,
            "gw_attack",
            gw_attack_then(self.session_actor(actor), target_guid)
        )
    }

    fn stop_attack(&self, actor: Actor) -> Result<()> {
        let coord = self.0.call_pipe();
        call_reducer!(
            coord.conn.reducers,
            "gw_stop_attack",
            gw_stop_attack_then(self.session_actor(actor))
        )
    }
}
