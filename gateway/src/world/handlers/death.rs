use super::super::*;

pub(crate) trait DeathStore: Send + Sync {
    fn repop(&self, actor: Actor) -> Result<()>;

    fn reclaim_corpse(&self, actor: Actor, corpse_guid: u64) -> Result<()>;

    fn resurrect_response(&self, actor: Actor, accept: bool) -> Result<()>;

    fn self_resurrect(&self, actor: Actor) -> Result<()>;

    fn spirit_healer_res(&self, actor: Actor, healer_guid: u64) -> Result<()>;

    fn corpse_location(&self, owner_guid: u64) -> Result<Option<(u32, f32, f32, f32)>>;
}
