use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorldSessionToken {
    pub account_id: u64,
    pub generation: u64,
    pub request_nonce: u128,
}

#[derive(Debug, PartialEq, Eq)]
pub enum InteractionOutcome {
    Done,
    Refused(String),
}

pub trait WorldStore:
    ShardRoutingStore
    + SessionStore
    + CharacterStore
    + TransferStore
    + PartyStore
    + LootWindowStore
    + LootRollStore
    + MailStore
    + SocialStore
    + NpcStore
    + TrainerStore
    + BankStore
    + CombatStore
    + DeathStore
    + TradeStore
    + AuctionActionStore
    + CastStore
    + ChannelActionStore
    + ChatActionStore
    + SpeechStore
    + DuelActionStore
    + GuildActionStore
    + ItemActionStore
    + MeetingStoneActionStore
    + MeleeActionStore
    + MemberStatsStore
    + QuestActionStore
    + TaxiActionStore
    + VendorActionStore
    + WeatherStore
    + Send
    + Sync
{
}

impl<T> WorldStore for T where
    T: ShardRoutingStore
        + SessionStore
        + CharacterStore
        + TransferStore
        + PartyStore
        + LootWindowStore
        + LootRollStore
        + MailStore
        + SocialStore
        + NpcStore
        + TrainerStore
        + BankStore
        + CombatStore
        + DeathStore
        + TradeStore
        + AuctionActionStore
        + CastStore
        + ChannelActionStore
        + ChatActionStore
        + SpeechStore
        + DuelActionStore
        + GuildActionStore
        + ItemActionStore
        + MeetingStoneActionStore
        + MeleeActionStore
        + MemberStatsStore
        + QuestActionStore
        + TaxiActionStore
        + VendorActionStore
        + WeatherStore
        + Send
        + Sync
{
}

pub(crate) trait ShardRoutingStore: Send + Sync {
    fn shard_name(&self) -> &str;

    fn settle_home_shard(
        &self,
        character_guid: u64,
    ) -> Result<Option<std::sync::Arc<dyn WorldStore>>>;

    fn shard_for_location(
        &self,
        map_id: u32,
        instance_id: u64,
    ) -> Option<std::sync::Arc<dyn WorldStore>>;

    fn bind_shard_session(&self, account_id: u64, session_key: &[u8; 40]) -> Result<()>;

    fn realm_store(&self) -> Result<Option<std::sync::Arc<dyn WorldStore>>>;

    fn world_stores(&self) -> Vec<std::sync::Arc<dyn WorldStore>>;

    fn party_command_worlds(&self) -> Result<Vec<std::sync::Arc<dyn WorldStore>>>;
}

pub(crate) trait SessionStore: Send + Sync {
    fn lookup_session(&self, account_name: &str) -> Result<Option<WorldSession>>;

    fn pending_system_messages(&self, self_guid: u64) -> Vec<String>;

    fn player_login(
        &self,
        account_id: u64,
        character: Actor,
        entry: codec::WorldEntry,
    ) -> Result<codec::EntityView>;

    fn movement_update(&self, actor: Actor, opcode: u32, info: &MovementInfo) -> Result<()>;

    fn subscribe_player_events(
        &self,
        account_id: u64,
        self_guid: u64,
        arrival: &codec::EntityView,
        tx: SessionTx,
    ) -> Result<PlayerSubscriptions>;

    fn client_command(&self, actor: Actor, cmd: String, payload: String) -> Result<()>;

    fn entity_in_world(&self, guid: u64) -> bool;

    fn entity_max_health(&self, guid: u64) -> u32;

    fn claim_session(&self, account_id: u64, character_guid: u64) -> Result<WorldSessionToken>;

    fn bind_session(
        &self,
        token: WorldSessionToken,
    ) -> Result<Option<std::sync::Arc<dyn WorldStore>>>;

    /// Close the matching World Shard fences before releasing the Realm-core claim.
    fn release_session(&self, token: WorldSessionToken) -> Result<()>;

    fn player_combat_until_ms(&self, player_guid: u64) -> u64;
}
