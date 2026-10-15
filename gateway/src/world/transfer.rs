use anyhow::{anyhow, Result};

use super::{party, Actor, WorldStore};

pub(crate) trait TransferStore: Send + Sync {
    fn escrowed_transfer(&self, character_guid: u64) -> Option<EscrowedTransfer>;

    fn character_destination(&self, character_guid: u64) -> Option<TransferPlan>;

    fn begin_transfer(&self, character: Actor, plan: &TransferPlan) -> Result<()>;

    fn import_character_blob(
        &self,
        transfer_id: u64,
        blob: &[u8],
        source: RealmLocatorPredecessor,
        bot_arrival: Option<&BotTransferIntent>,
    ) -> Result<()>;

    fn confirm_import(&self, transfer_id: u64) -> Result<()>;

    fn finish_transfer(&self, transfer_id: u64) -> Result<()>;

    fn release_transfer(&self, transfer_id: u64) -> Result<()>;

    fn release_player_transfer_arrival(
        &self,
        transfer_id: u64,
        character_guid: u64,
        source: RealmLocatorPredecessor,
    ) -> Result<()>;

    fn transfer_arrival(&self, transfer_id: u64) -> Option<TransferArrival>;

    fn sync_transfer_arrival(&self, character_guid: u64) -> Result<()>;

    fn sync_transfer_pending(&self, character_guid: u64) -> Result<()>;

    fn begin_shard_index_transfer(
        &self,
        plan: &TransferPlan,
        bot_intent: Option<(&BotTransferIntent, u64)>,
    ) -> Result<party::RealmCharacterPartition>;

    fn finish_player_shard_index_transfer(
        &self,
        plan: &TransferPlan,
        source_map: u32,
        source_instance: u64,
        source_revision: u64,
    ) -> Result<()>;

    fn finish_pending_shard_index_transfer(
        &self,
        character_guid: u64,
        destination_map: u32,
        destination_instance: u64,
        arrival: &TransferArrival,
    ) -> Result<()>;

    fn bind_bot_transfer_locator(
        &self,
        intent: &BotTransferIntent,
        source_revision: u64,
        claim_token: u64,
    ) -> Result<()>;

    fn publish_bot_shard_index(&self, intent: &BotTransferIntent) -> Result<()>;

    fn instance_partition(&self, instance_id: u64) -> Option<(u32, u64)>;

    fn ensure_instance(&self, instance_id: u64, map_id: u32, party_id: u64) -> Result<()>;

    fn evict_instance_population(&self, instance_id: u64) -> Result<()>;

    fn mark_bot_transfer_arrival_ready(
        &self,
        intent_id: u64,
        bot_guid: u64,
        controller_generation: u64,
        claim_token: u64,
    ) -> Result<()>;

    fn bot_transfer_arrival_matches(&self, transfer_id: u64, intent: &BotTransferIntent) -> bool;

    fn release_bot_transfer_arrival(
        &self,
        transfer_id: u64,
        intent: &BotTransferIntent,
    ) -> Result<()>;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransferPlan {
    pub transfer_id: u64,
    pub character_guid: u64,
    pub dest_map_id: u32,
    pub dest_instance_id: u64,
    pub dest_x: f32,
    pub dest_y: f32,
    pub dest_z: f32,
    pub dest_o: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EscrowedTransfer {
    pub transfer_id: u64,
    pub character_guid: u64,
    pub dest_map_id: u32,
    pub dest_instance_id: u64,
    pub blob: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransferArrival {
    pub character_guid: u64,
    pub source_map: u32,
    pub source_instance: u64,
    pub source_locator_revision: u64,
    pub bot_source_identity: spacetimedb_sdk::Identity,
    pub bot_transfer_intent_id: u64,
    pub bot_controller_generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RealmLocatorPredecessor {
    pub map_id: u32,
    pub instance_id: u64,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotTransferIntent {
    pub id: u64,
    pub bot_guid: u64,
    pub destination_map: u32,
    pub destination_instance: u64,
    pub reason: String,
    pub created_micros: i64,
    pub controller_generation: u64,
    pub arrival_ready: bool,
    pub source_module_identity: spacetimedb_sdk::Identity,
    pub source_map: u32,
    pub source_instance: u64,
    pub source_locator_revision: u64,
}

pub fn transfer_id_for(character_guid: u64) -> u64 {
    character_guid
}

pub const ABORT_STEPS: [&str; 10] = [
    "sync_transfer_pending",
    "begin_transfer",
    "ensure_instance",
    "import_character_blob",
    "confirm_import",
    "finish_transfer",
    "publish_shard_index",
    "sync_transfer_arrival",
    "release_transfer",
    "evict_instance_population",
];

pub const BOT_ABORT_STEPS: [&str; 12] = [
    "bind_bot_transfer_locator",
    "sync_transfer_pending",
    "begin_transfer",
    "ensure_instance",
    "import_character_blob",
    "confirm_import",
    "finish_transfer",
    "publish_shard_index",
    "sync_transfer_arrival",
    "mark_bot_transfer_arrival_ready",
    "release_transfer",
    "evict_instance_population",
];

#[cfg(not(test))]
fn die_by_injection() -> ! {
    log::logger().flush();
    std::process::abort()
}

#[cfg(test)]
fn die_by_injection() -> ! {
    panic!("LYRACORE_TRANSFER_ABORT_AFTER: injected abort");
}

#[inline]
fn abort_point(abort_after: Option<&str>, step: &str, transfer_id: u64) {
    if abort_after != Some(step) {
        return;
    }
    log::error!(
        "transfer {transfer_id}: LYRACORE_TRANSFER_ABORT_AFTER={step} — step committed, ABORTING BY \
         FAULT INJECTION (the gateway-kill recovery test). This is a deliberate crash, not a bug."
    );
    die_by_injection()
}

pub fn run_transfer(src: &dyn WorldStore, dst: &dyn WorldStore, plan: &TransferPlan) -> Result<()> {
    let abort_after = std::env::var("LYRACORE_TRANSFER_ABORT_AFTER").ok();
    if let Some(step) = abort_after.as_deref() {
        if !ABORT_STEPS.contains(&step) {
            log::error!(
                "LYRACORE_TRANSFER_ABORT_AFTER={step} names no transfer step — NOTHING will abort. \
                 Valid steps: {ABORT_STEPS:?}"
            );
        }
    }
    run_transfer_injected(src, dst, plan, abort_after.as_deref())
}

// Call-pipe completion can precede the Escrow row reaching the Coordinator cache.
const ESCROW_VISIBLE_WITHIN: std::time::Duration = std::time::Duration::from_secs(3);
const ESCROW_POLL: std::time::Duration = std::time::Duration::from_millis(20);

fn escrow_after_begin(src: &dyn WorldStore, plan: &TransferPlan) -> Result<EscrowedTransfer> {
    let deadline = std::time::Instant::now() + ESCROW_VISIBLE_WITHIN;
    loop {
        if let Some(escrow) = src.escrowed_transfer(plan.character_guid) {
            return Ok(escrow);
        }
        if std::time::Instant::now() >= deadline {
            return Err(anyhow!(
                "transfer {}: begin_transfer reported success but no escrow row is readable on {} \
                 within {ESCROW_VISIBLE_WITHIN:?} — refusing to import a character whose source \
                 copy is not frozen",
                plan.transfer_id,
                src.shard_name()
            ));
        }
        std::thread::sleep(ESCROW_POLL);
    }
}

pub(super) fn run_transfer_injected(
    src: &dyn WorldStore,
    dst: &dyn WorldStore,
    plan: &TransferPlan,
    abort_after: Option<&str>,
) -> Result<()> {
    run_transfer_injected_for_intent(src, dst, plan, abort_after, None)
}

pub(super) fn run_transfer_injected_for_intent(
    src: &dyn WorldStore,
    dst: &dyn WorldStore,
    plan: &TransferPlan,
    abort_after: Option<&str>,
    bot_intent: Option<(&BotTransferIntent, u64)>,
) -> Result<()> {
    let resumed = src
        .escrowed_transfer(plan.character_guid)
        .map(|escrow| {
            if (escrow.transfer_id, escrow.character_guid)
                != (plan.transfer_id, plan.character_guid)
            {
                return Err(anyhow!(
                    "transfer {}: source escrow identity changed",
                    plan.transfer_id
                ));
            }
            Ok(TransferPlan {
                transfer_id: escrow.transfer_id,
                character_guid: escrow.character_guid,
                dest_map_id: escrow.dest_map_id,
                dest_instance_id: escrow.dest_instance_id,
                dest_x: 0.0,
                dest_y: 0.0,
                dest_z: 0.0,
                dest_o: 0.0,
            })
        })
        .transpose()?;
    let plan = resumed.as_ref().unwrap_or(plan);
    log::info!(
        "transfer {}: character {} {} -> {} (map {} instance {})",
        plan.transfer_id,
        plan.character_guid,
        src.shard_name(),
        dst.shard_name(),
        plan.dest_map_id,
        plan.dest_instance_id
    );

    let owned_bot_intent = bot_intent
        .map(|(intent, token)| {
            bind_bot_locator(src, intent, token, abort_after).map(|intent| (intent, token))
        })
        .transpose()?;
    let locator = src.begin_shard_index_transfer(
        plan,
        owned_bot_intent
            .as_ref()
            .map(|(intent, token)| (intent, *token)),
    )?;
    if !locator.transfer_pending {
        return Err(anyhow!(
            "cross-Shard Transfer Realm phase is already settled"
        ));
    }
    // Publish PendingTransfer before freezing; same-revision Known snapshots then lose.
    src.sync_transfer_pending(plan.character_guid)?;
    abort_point(abort_after, "sync_transfer_pending", plan.transfer_id);
    let bot_intent = owned_bot_intent
        .as_ref()
        .map(|(intent, token)| (intent, *token));

    let character = Actor::new(plan.character_guid)
        .ok_or_else(|| anyhow!("transfer {}: the plan names no Character", plan.transfer_id))?;
    src.begin_transfer(character, plan)?;
    abort_point(abort_after, "begin_transfer", plan.transfer_id);

    let escrow = escrow_after_begin(src, plan)?;

    // Mirror the source lease before import; its admitted Party cannot change during the crossing.
    if escrow.dest_instance_id != 0 {
        let (source_map, party_id) =
            src.instance_partition(escrow.dest_instance_id)
                .ok_or_else(|| {
                    anyhow!(
                        "transfer {}: source instance {} has no durable lease",
                        escrow.transfer_id,
                        escrow.dest_instance_id
                    )
                })?;
        if source_map != escrow.dest_map_id {
            return Err(anyhow!(
                "transfer {}: source instance {} belongs to map {source_map}, not destination map {}",
                escrow.transfer_id,
                escrow.dest_instance_id,
                escrow.dest_map_id
            ));
        }
        dst.ensure_instance(escrow.dest_instance_id, escrow.dest_map_id, party_id)?;
    }
    abort_point(abort_after, "ensure_instance", escrow.transfer_id);

    let source = RealmLocatorPredecessor {
        map_id: locator.map_id,
        instance_id: locator.instance_id,
        revision: locator.revision,
    };
    dst.import_character_blob(
        escrow.transfer_id,
        &escrow.blob,
        source,
        bot_intent.map(|(intent, _)| intent),
    )?;
    abort_point(abort_after, "import_character_blob", escrow.transfer_id);

    // Attest only after destination import succeeds; attestation licenses source deletion.
    src.confirm_import(escrow.transfer_id)?;
    abort_point(abort_after, "confirm_import", escrow.transfer_id);

    src.finish_transfer(escrow.transfer_id)?;
    abort_point(abort_after, "finish_transfer", escrow.transfer_id);

    if let Some((intent, _)) = bot_intent {
        src.publish_bot_shard_index(intent)?;
    } else {
        src.finish_player_shard_index_transfer(
            plan,
            locator.map_id,
            locator.instance_id,
            locator.revision,
        )?;
    }
    abort_point(abort_after, "publish_shard_index", escrow.transfer_id);

    // Keep the arrival fence until the destination has the authoritative Party mirror.
    dst.sync_transfer_arrival(escrow.character_guid)?;
    abort_point(abort_after, "sync_transfer_arrival", escrow.transfer_id);

    // Persist the session-less driver's destination-ready witness before releasing its fence.
    if let Some((intent, claim_token)) = bot_intent {
        src.mark_bot_transfer_arrival_ready(
            intent.id,
            intent.bot_guid,
            intent.controller_generation,
            claim_token,
        )?;
        abort_point(
            abort_after,
            "mark_bot_transfer_arrival_ready",
            escrow.transfer_id,
        );
    }

    if let Some((intent, _)) = bot_intent {
        dst.release_bot_transfer_arrival(escrow.transfer_id, intent)?;
    } else {
        dst.release_player_transfer_arrival(escrow.transfer_id, escrow.character_guid, source)?;
    }
    abort_point(abort_after, "release_transfer", escrow.transfer_id);

    evict_finished_instance(src, escrow.transfer_id, escrow.dest_instance_id);
    abort_point(abort_after, "evict_instance_population", escrow.transfer_id);
    Ok(())
}

fn evict_finished_instance(source: &dyn WorldStore, transfer_id: u64, instance_id: u64) {
    if instance_id == 0 {
        return;
    }
    if let Err(error) = source.evict_instance_population(instance_id) {
        log::warn!(
            "transfer {transfer_id}: could not evict instance {instance_id} population from {} \
             ({error:#}); the run is complete, but that shard keeps ticking an empty dungeon until \
             the reaper takes it",
            source.shard_name()
        );
    }
}

#[cfg(test)]
pub fn run_bot_transfer(
    holder: &dyn WorldStore,
    bot_guid: u64,
    destination_map: u32,
    destination_instance: u64,
    reason: &str,
) -> Result<()> {
    let plan = holder.character_destination(bot_guid).ok_or_else(|| {
        anyhow!(
            "bot transfer: {} holds no character {bot_guid} to move (map {destination_map} \
             instance {destination_instance}, {reason})",
            holder.shard_name()
        )
    })?;
    if (plan.dest_map_id, plan.dest_instance_id) != (destination_map, destination_instance) {
        return Err(anyhow!(
            "bot transfer: character {bot_guid} on {} is bound for map {} instance {}, but the \
             intent asks for map {destination_map} instance {destination_instance} ({reason}) — \
             refusing, because the module places a bot before it records the crossing, so this row \
             is stale or its bot never moved",
            holder.shard_name(),
            plan.dest_map_id,
            plan.dest_instance_id
        ));
    }
    let Some(destination) = holder.shard_for_location(destination_map, destination_instance) else {
        log::debug!(
            "bot transfer: {} already serves map {destination_map} instance \
             {destination_instance} — character {bot_guid} is there ({reason})",
            holder.shard_name()
        );
        return Ok(());
    };
    log::info!(
        "bot transfer: character {bot_guid} {} -> {} (map {destination_map} instance \
         {destination_instance}, {reason})",
        holder.shard_name(),
        destination.shard_name()
    );
    settle_transfer(holder, destination.as_ref(), bot_guid)
}

pub fn run_bot_transfer_intent(
    holder: &dyn WorldStore,
    intent: &BotTransferIntent,
    claim_token: u64,
) -> Result<()> {
    let abort_after = std::env::var("LYRACORE_TRANSFER_ABORT_AFTER").ok();
    if let Some(step) = abort_after.as_deref() {
        if !BOT_ABORT_STEPS.contains(&step) {
            log::error!(
                "LYRACORE_TRANSFER_ABORT_AFTER={step} names no bot Transfer step; valid steps: \
                 {BOT_ABORT_STEPS:?}"
            );
        }
    }
    run_bot_transfer_intent_injected(holder, intent, claim_token, abort_after.as_deref())
}

pub(super) fn run_bot_transfer_intent_injected(
    holder: &dyn WorldStore,
    intent: &BotTransferIntent,
    claim_token: u64,
    abort_after: Option<&str>,
) -> Result<()> {
    let transfer_id = transfer_id_for(intent.bot_guid);
    if intent.arrival_ready {
        let routed = holder.shard_for_location(intent.destination_map, intent.destination_instance);
        let destination = match routed.as_deref() {
            Some(destination) => destination,
            None => {
                let local = holder.character_destination(intent.bot_guid);
                if !local.is_some_and(|plan| {
                    (plan.dest_map_id, plan.dest_instance_id)
                        == (intent.destination_map, intent.destination_instance)
                }) {
                    return Err(anyhow!(
                        "bot transfer: ready intent {} cannot resolve its destination map {} instance {}",
                        intent.id,
                        intent.destination_map,
                        intent.destination_instance
                    ));
                }
                holder
            }
        };
        destination.release_bot_transfer_arrival(transfer_id, intent)?;
        abort_point(abort_after, "release_transfer", transfer_id);
        if routed.is_some() {
            evict_finished_instance(holder, transfer_id, intent.destination_instance);
            abort_point(abort_after, "evict_instance_population", transfer_id);
        }
        return Ok(());
    }
    if let Some(plan) = holder.character_destination(intent.bot_guid) {
        if (plan.dest_map_id, plan.dest_instance_id)
            != (intent.destination_map, intent.destination_instance)
        {
            return Err(anyhow!(
                "bot transfer: character {} on {} is bound for map {} instance {}, but intent {} \
                 asks for map {} instance {} ({})",
                intent.bot_guid,
                holder.shard_name(),
                plan.dest_map_id,
                plan.dest_instance_id,
                intent.id,
                intent.destination_map,
                intent.destination_instance,
                intent.reason
            ));
        }
        let Some(destination) =
            holder.shard_for_location(intent.destination_map, intent.destination_instance)
        else {
            let (bound, settled) =
                prepare_bot_locator(holder, &plan, intent, claim_token, abort_after)?;
            if !settled {
                holder.publish_bot_shard_index(&bound)?;
                abort_point(abort_after, "publish_shard_index", transfer_id);
            }
            holder.sync_transfer_arrival(intent.bot_guid)?;
            abort_point(abort_after, "sync_transfer_arrival", transfer_id);
            holder.mark_bot_transfer_arrival_ready(
                intent.id,
                intent.bot_guid,
                intent.controller_generation,
                claim_token,
            )?;
            abort_point(abort_after, "mark_bot_transfer_arrival_ready", transfer_id);
            return Ok(());
        };
        return run_transfer_injected_for_intent(
            holder,
            destination.as_ref(),
            &plan,
            abort_after,
            Some((intent, claim_token)),
        );
    }
    let destination = holder
        .shard_for_location(intent.destination_map, intent.destination_instance)
        .ok_or_else(|| {
            anyhow!(
                "bot transfer: {} has no source character {} and no distinct destination for map \
                 {} instance {}",
                holder.shard_name(),
                intent.bot_guid,
                intent.destination_map,
                intent.destination_instance
            )
        })?;
    let settled = destination
        .character_destination(intent.bot_guid)
        .ok_or_else(|| {
            anyhow!(
                "bot transfer: neither {} nor {} holds character {}",
                holder.shard_name(),
                destination.shard_name(),
                intent.bot_guid
            )
        })?;
    if (settled.dest_map_id, settled.dest_instance_id)
        != (intent.destination_map, intent.destination_instance)
    {
        return Err(anyhow!(
            "bot transfer: destination {} holds character {} at map {} instance {}, but intent {} \
             names map {} instance {}",
            destination.shard_name(),
            intent.bot_guid,
            settled.dest_map_id,
            settled.dest_instance_id,
            intent.id,
            intent.destination_map,
            intent.destination_instance
        ));
    }
    if !destination.bot_transfer_arrival_matches(transfer_id, intent) {
        return Err(anyhow!(
            "bot transfer: destination {} does not hold the exact arrival fence for source {} \
             intent {} generation {}",
            destination.shard_name(),
            intent.source_module_identity,
            intent.id,
            intent.controller_generation
        ));
    }
    holder.publish_bot_shard_index(intent)?;
    abort_point(abort_after, "publish_shard_index", transfer_id);
    destination.sync_transfer_arrival(intent.bot_guid)?;
    abort_point(abort_after, "sync_transfer_arrival", transfer_id);
    holder.mark_bot_transfer_arrival_ready(
        intent.id,
        intent.bot_guid,
        intent.controller_generation,
        claim_token,
    )?;
    abort_point(abort_after, "mark_bot_transfer_arrival_ready", transfer_id);
    destination.release_bot_transfer_arrival(transfer_id, intent)?;
    abort_point(abort_after, "release_transfer", transfer_id);
    Ok(())
}

fn prepare_bot_locator(
    holder: &dyn WorldStore,
    plan: &TransferPlan,
    intent: &BotTransferIntent,
    claim_token: u64,
    abort_after: Option<&str>,
) -> Result<(BotTransferIntent, bool)> {
    let bound = bind_bot_locator(holder, intent, claim_token, abort_after)?;
    let locator = holder.begin_shard_index_transfer(plan, Some((&bound, claim_token)))?;
    if locator.transfer_pending {
        holder.sync_transfer_pending(intent.bot_guid)?;
        abort_point(
            abort_after,
            "sync_transfer_pending",
            transfer_id_for(intent.bot_guid),
        );
    }
    Ok((bound, !locator.transfer_pending))
}

fn bind_bot_locator(
    holder: &dyn WorldStore,
    intent: &BotTransferIntent,
    claim_token: u64,
    abort_after: Option<&str>,
) -> Result<BotTransferIntent> {
    if intent.source_locator_revision != 0 {
        return Ok(intent.clone());
    }
    let realm = holder.realm_store()?;
    let source_revision = realm
        .as_deref()
        .unwrap_or(holder)
        .realm_character_partition(intent.bot_guid)?
        .filter(|row| {
            !row.transfer_pending
                && (row.map_id, row.instance_id) == (intent.source_map, intent.source_instance)
        })
        .ok_or_else(|| anyhow!("bot Transfer Realm source locator changed"))?
        .revision;
    holder.bind_bot_transfer_locator(intent, source_revision, claim_token)?;
    abort_point(
        abort_after,
        "bind_bot_transfer_locator",
        transfer_id_for(intent.bot_guid),
    );
    let mut bound = intent.clone();
    bound.source_locator_revision = source_revision;
    Ok(bound)
}

pub fn settle_transfer(
    holder: &dyn WorldStore,
    owner: &dyn WorldStore,
    character_guid: u64,
) -> Result<()> {
    let transfer_id = transfer_id_for(character_guid);
    if holder.shard_name() == owner.shard_name() {
        let arrival = owner.transfer_arrival(transfer_id);
        if let Some(arrival) = arrival {
            if arrival.bot_transfer_intent_id != 0 {
                return Err(anyhow!(
                    "session-less arrival is still owned by its Transfer Intent"
                ));
            }
            if arrival.source_locator_revision != 0 {
                let destination = owner
                    .character_destination(character_guid)
                    .ok_or_else(|| anyhow!("arrival has no destination Character"))?;
                owner.finish_pending_shard_index_transfer(
                    character_guid,
                    destination.dest_map_id,
                    destination.dest_instance_id,
                    &arrival,
                )?;
            }
            owner.sync_transfer_arrival(character_guid)?;
            if arrival.source_locator_revision != 0 {
                return owner.release_player_transfer_arrival(
                    transfer_id,
                    character_guid,
                    RealmLocatorPredecessor {
                        map_id: arrival.source_map,
                        instance_id: arrival.source_instance,
                        revision: arrival.source_locator_revision,
                    },
                );
            }
        }
        return owner.release_transfer(transfer_id);
    }
    let escrow = holder.escrowed_transfer(character_guid);
    if escrow.is_none() {
        holder.release_transfer(transfer_id)?;
    }
    let plan = match escrow {
        Some(e) => TransferPlan {
            transfer_id: e.transfer_id,
            character_guid,
            dest_map_id: e.dest_map_id,
            dest_instance_id: e.dest_instance_id,
            dest_x: 0.0,
            dest_y: 0.0,
            dest_z: 0.0,
            dest_o: 0.0,
        },
        None => holder
            .character_destination(character_guid)
            .ok_or_else(|| {
                anyhow!(
                    "character {character_guid} is not on {} and has no escrow there — nothing to \
                 transfer from",
                    holder.shard_name()
                )
            })?,
    };
    run_transfer(holder, owner, &plan)
}
