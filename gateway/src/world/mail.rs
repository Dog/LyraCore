use crate::world::{CharacterStore, SessionStore, ShardRoutingStore, SocialStore};
use anyhow::Result;

use super::{presence, Actor, WorldStore};
use crate::codec::MailView;
use crate::stdb::{classify, DurableFailure};
use lyracore_shared::mail as mail_rules;

pub(crate) trait MailStore: Send + Sync {
    fn mail_list(&self, recipient_guid: u64) -> Result<Vec<MailView>>;

    fn mail_by_id(&self, mail_id: u64) -> Result<Option<MailView>>;

    fn realm_account_name(&self, character_guid: u64) -> Result<Option<String>>;

    fn mailbox_in_range(&self, mailbox_guid: u64, player_guid: u64) -> Result<bool>;

    fn mail_mark_read(&self, recipient: Actor, mail_id: u64) -> Result<()>;

    fn mail_delete(&self, recipient: Actor, mail_id: u64) -> Result<()>;

    fn mail_return(&self, recipient: Actor, mail_id: u64, same_account: bool) -> Result<()>;

    #[allow(clippy::too_many_arguments)]
    fn mail_send(
        &self,
        sender: Actor,
        recipient_guid: u64,
        subject: String,
        body: String,
        money: u32,
        cod: u32,
        item_guid: u64,
        same_account: bool,
    ) -> Result<()>;

    fn mail_take_money(&self, recipient: Actor, mail_id: u64) -> Result<()>;

    fn mail_take_item(&self, recipient: Actor, mail_id: u64) -> Result<()>;

    fn mail_item_room(&self, payee: Actor) -> Result<()>;

    fn mail_copy_text(&self, recipient: Actor, mail_id: u64) -> Result<()>;

    fn mail_grant_letter(&self, payee: Actor, item_text_id: u32) -> Result<()>;

    fn mail_mark_letter_granted(&self, recipient: Actor, mail_id: u64) -> Result<()>;

    fn item_text(&self, item_text_id: u32) -> Result<Option<String>>;

    fn owns_item_with_text(
        &self,
        owner_guid: u64,
        item_text_id: u32,
        hint_item_guid: u64,
    ) -> Result<bool>;

    #[allow(clippy::too_many_arguments)]
    fn mail_fence(
        &self,
        escrow_id: u64,
        sender: Actor,
        recipient_guid: u64,
        subject: String,
        body: String,
        money: u32,
        postage: u32,
        item_guid: u64,
        cod: u32,
        cod_source_mail_id: u64,
        same_account: bool,
    ) -> Result<()>;

    #[allow(clippy::too_many_arguments)]
    fn mail_commit(
        &self,
        escrow_id: u64,
        sender: Actor,
        recipient_guid: u64,
        subject: String,
        body: String,
        money: u32,
        item: AttachedItem,
        cod: u32,
        cod_source_mail_id: u64,
        delivery_delay_secs: u32,
        reward: Option<lyracore_shared::mail::RewardHeader>,
    ) -> Result<()>;

    fn mail_take_money_fence(
        &self,
        escrow_id: u64,
        payee: Actor,
        mail_id: u64,
        expect_money: u32,
    ) -> Result<()>;

    fn mail_payout(&self, escrow_id: u64, payee: Actor, mail_id: u64, amount: u32) -> Result<()>;

    fn mail_take_item_fence(
        &self,
        escrow_id: u64,
        payee: Actor,
        mail_id: u64,
        expect_entry: u32,
    ) -> Result<()>;

    fn mail_item_payout(
        &self,
        escrow_id: u64,
        payee: Actor,
        mail_id: u64,
        item: AttachedItem,
    ) -> Result<()>;

    fn mail_confirm_delivery(&self, escrow_id: u64) -> Result<()>;

    fn mail_settle(&self, escrow_id: u64) -> Result<()>;

    fn mail_escrows_of(&self, sender_guid: u64) -> Result<Vec<HeldEscrow>>;
}

struct EscrowIdRange {
    next: std::sync::atomic::AtomicU64,
    start: u64,
    end: u64,
}

static ESCROW_ID_RANGE: std::sync::OnceLock<EscrowIdRange> = std::sync::OnceLock::new();
const NO_COD_SOURCE: u64 = 0;
const NO_DELIVERY_DELAY: u32 = 0;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SendRefusal {
    NoMailbox(String),
    RecipientNotFound(String),
    CannotSendToSelf,
    NotYourTeam,
    NotEnoughMoney(String),
    AttachmentInvalid(String),
    AttachmentSoulbound(String),
    Internal(String),
}

impl std::fmt::Display for SendRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoMailbox(e)
            | Self::RecipientNotFound(e)
            | Self::NotEnoughMoney(e)
            | Self::AttachmentInvalid(e)
            | Self::AttachmentSoulbound(e) => f.write_str(e),
            Self::CannotSendToSelf => f.write_str(mail_rules::CANNOT_SEND_TO_SELF),
            Self::NotYourTeam => f.write_str(mail_rules::NOT_YOUR_TEAM),
            Self::Internal(e) => f.write_str(e),
        }
    }
}

impl FromReason for SendRefusal {
    fn from_reason(reason: &str) -> Self {
        let reason = reason.to_string();
        if reason.contains(mail_rules::NOT_ENOUGH_MONEY) {
            SendRefusal::NotEnoughMoney(reason)
        } else if reason.contains(mail_rules::ITEM_IS_SOULBOUND) {
            SendRefusal::AttachmentSoulbound(reason)
        } else if reason.contains(mail_rules::NOT_YOUR_ITEM) {
            SendRefusal::AttachmentInvalid(reason)
        } else {
            SendRefusal::Internal(reason)
        }
    }
}

#[derive(Debug)]
pub(crate) enum MailFailure<R> {
    Refused(R),
    Lost(anyhow::Error),
}

impl<R> MailFailure<R> {
    pub(crate) fn refusal(self) -> Result<R> {
        match self {
            Self::Refused(refusal) => Ok(refusal),
            Self::Lost(error) => Err(error),
        }
    }

    #[cfg(test)]
    pub(crate) fn into_refusal(self) -> R {
        match self {
            Self::Refused(refusal) => refusal,
            Self::Lost(error) => panic!("expected a Refusal, got a Transport Loss: {error:#}"),
        }
    }

    fn map<S>(self, f: impl FnOnce(R) -> S) -> MailFailure<S> {
        match self {
            Self::Refused(refusal) => MailFailure::Refused(f(refusal)),
            Self::Lost(error) => MailFailure::Lost(error),
        }
    }
}

impl<R: std::fmt::Display> std::fmt::Display for MailFailure<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(refusal) => refusal.fmt(f),
            Self::Lost(error) => write!(f, "{error:#}"),
        }
    }
}

pub(crate) trait FromReason {
    fn from_reason(reason: &str) -> Self;
}

impl FromReason for String {
    fn from_reason(reason: &str) -> Self {
        reason.to_string()
    }
}

impl<R: FromReason> From<anyhow::Error> for MailFailure<R> {
    fn from(error: anyhow::Error) -> Self {
        match classify(&error) {
            DurableFailure::Refusal { reason } => Self::Refused(R::from_reason(reason)),
            DurableFailure::TransportLoss => Self::Lost(error),
        }
    }
}

pub(crate) type MailResult<T, R = String> = std::result::Result<T, MailFailure<R>>;

fn refused<R: FromReason>(reason: &str) -> MailFailure<R> {
    MailFailure::Refused(R::from_reason(reason))
}

pub(crate) fn mail_of<St: MailStore + ShardRoutingStore + ?Sized>(
    store: &St,
    self_guid: u64,
) -> Result<Vec<MailView>> {
    let mails = match store.realm_store()? {
        Some(realm) => realm.mail_list(self_guid)?,
        None => store.mail_list(self_guid)?,
    };
    let now = now_secs();
    Ok(mails.into_iter().filter(|m| m.is_delivered(now)).collect())
}
pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
pub(crate) fn open_mailbox<St: MailStore + ShardRoutingStore + ?Sized>(
    store: &St,
    actor: Option<Actor>,
    mailbox_guid: u64,
) -> MailResult<Vec<MailView>> {
    let actor = at_mailbox(store, actor, mailbox_guid)?;
    redrive(store, actor.guid());
    Ok(mail_of(store, actor.guid())?)
}
pub(crate) fn has_unread<St: MailStore + ShardRoutingStore + ?Sized>(
    store: &St,
    actor: Option<Actor>,
) -> MailResult<bool> {
    let actor = actor.ok_or_else(|| refused(mail_rules::NOT_IN_WORLD))?;
    Ok(mail_of(store, actor.guid())?.iter().any(|m| !m.was_read))
}
pub(crate) fn letter_body<St: MailStore + ShardRoutingStore + ?Sized>(
    store: &St,
    actor: Option<Actor>,
    mail_id: u64,
) -> MailResult<Option<String>> {
    let actor = actor.ok_or_else(|| refused(mail_rules::NOT_IN_WORLD))?;
    Ok(mail_of(store, actor.guid())?
        .into_iter()
        .find(|m| m.id == mail_id)
        .map(|m| m.body))
}
pub(crate) fn mark_read<St: MailStore + ShardRoutingStore + ?Sized>(
    store: &St,
    actor: Option<Actor>,
    mailbox_guid: u64,
    mail_id: u64,
) -> MailResult<()> {
    let actor = at_mailbox(store, actor, mailbox_guid)?;
    match store.realm_store()? {
        Some(realm) => realm.mail_mark_read(actor, mail_id),
        None => store.mail_mark_read(actor, mail_id),
    }?;
    Ok(())
}
pub(crate) fn delete<St: MailStore + ShardRoutingStore + ?Sized>(
    store: &St,
    actor: Option<Actor>,
    mailbox_guid: u64,
    mail_id: u64,
) -> MailResult<()> {
    let actor = at_mailbox(store, actor, mailbox_guid)?;
    match store.realm_store()? {
        Some(realm) => realm.mail_delete(actor, mail_id),
        None => store.mail_delete(actor, mail_id),
    }?;
    Ok(())
}
pub(crate) fn return_to_sender<St: MailStore + ShardRoutingStore + ?Sized>(
    store: &St,
    actor: Option<Actor>,
    mailbox_guid: u64,
    mail_id: u64,
) -> MailResult<()> {
    let actor = at_mailbox(store, actor, mailbox_guid)?;
    let realm = store.realm_store()?;
    let mail = match &realm {
        Some(realm) => realm.mail_by_id(mail_id)?,
        None => store.mail_by_id(mail_id)?,
    };
    let same_account = match mail {
        Some(mail) => same_realm_account(store, actor.guid(), mail.sender_guid)?,
        None => false,
    };
    match realm {
        Some(realm) => realm.mail_return(actor, mail_id, same_account),
        None => store.mail_return(actor, mail_id, same_account),
    }?;
    Ok(())
}
fn same_realm_account<St: MailStore + ShardRoutingStore + ?Sized>(
    store: &St,
    a: u64,
    b: u64,
) -> Result<bool> {
    Ok(
        match (
            realm_account_anywhere(store, a)?,
            realm_account_anywhere(store, b)?,
        ) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        },
    )
}
fn realm_account_anywhere<St: MailStore + ShardRoutingStore + ?Sized>(
    store: &St,
    guid: u64,
) -> Result<Option<String>> {
    if let Some(name) = store.realm_account_name(guid)? {
        return Ok(Some(name));
    }
    for shard in store.world_stores() {
        if let Some(name) = shard.realm_account_name(guid)? {
            return Ok(Some(name));
        }
    }
    Ok(None)
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn send<
    St: CharacterStore + MailStore + SessionStore + ShardRoutingStore + SocialStore + ?Sized,
>(
    store: &St,
    actor: Option<Actor>,
    mailbox_guid: u64,
    recipient_name: &str,
    subject: String,
    body: String,
    money: u32,
    cod: u32,
    item_guid: u64,
) -> MailResult<(), SendRefusal> {
    let sender_actor = at_mailbox::<St, String>(store, actor, mailbox_guid)
        .map_err(|failure| failure.map(SendRefusal::NoMailbox))?;
    let sender_guid = sender_actor.guid();
    if !presence::live_anywhere(store, sender_guid) {
        return Err(MailFailure::Refused(SendRefusal::NoMailbox(
            mail_rules::NOT_IN_WORLD.to_string(),
        )));
    }
    let candidates = presence::resolve_all_by_name(store, recipient_name)?;
    if candidates.is_empty() {
        return Err(MailFailure::Refused(SendRefusal::RecipientNotFound(
            mail_rules::no_recipient_named(recipient_name),
        )));
    }
    if candidates.contains(&sender_guid) {
        return Err(MailFailure::Refused(SendRefusal::CannotSendToSelf));
    }
    let sender = presence::character_anywhere(store, sender_guid)?.ok_or_else(|| {
        MailFailure::Refused(SendRefusal::Internal(mail_rules::NOT_IN_WORLD.to_string()))
    })?;
    let mut reachable = Vec::new();
    for guid in candidates {
        let Some(candidate) = presence::character_anywhere(store, guid)? else {
            continue;
        };
        if lyracore_shared::faction::same_team(sender.race, candidate.race) {
            reachable.push(guid);
        }
    }
    let recipient_guid = match reachable.as_slice() {
        [] => return Err(MailFailure::Refused(SendRefusal::NotYourTeam)),
        [only] => *only,
        _ => {
            return Err(MailFailure::Refused(SendRefusal::RecipientNotFound(
                mail_rules::ambiguous_recipient(recipient_name),
            )))
        }
    };
    let cod = mail_rules::cod_at_send(cod, item_guid != 0);
    let same_account = same_realm_account(store, sender_guid, recipient_guid)?;
    match store.realm_store()? {
        None => store.mail_send(
            sender_actor,
            recipient_guid,
            subject,
            body,
            money,
            cod,
            item_guid,
            same_account,
        )?,
        Some(realm) => {
            let escrow_id = next_escrow_id()?;
            store.mail_fence(
                escrow_id,
                sender_actor,
                recipient_guid,
                subject.clone(),
                body.clone(),
                money,
                mail_rules::postage(),
                item_guid,
                cod,
                NO_COD_SOURCE,
                same_account,
            )?;
            let held = held_fence(store, sender_guid, escrow_id)?;
            drive(store, escrow_id, || {
                realm.mail_commit(
                    escrow_id,
                    sender_actor,
                    recipient_guid,
                    subject,
                    body,
                    money,
                    held.item.clone(),
                    cod,
                    NO_COD_SOURCE,
                    held.delivery_delay_secs,
                    held.reward,
                )
            })
            .map_err(|error| MailFailure::<String>::from(error).map(SendRefusal::Internal))?;
        }
    }
    Ok(())
}
fn drive<St, F>(source: &St, escrow_id: u64, commit: F) -> Result<()>
where
    St: MailStore + ?Sized,
    F: FnOnce() -> Result<()>,
{
    commit()?;
    source.mail_confirm_delivery(escrow_id)?;
    source.mail_settle(escrow_id)
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AttachedItem {
    pub entry: u32,
    pub stack_count: u32,
    pub durability: u32,
    pub enchant_id: u32,
    pub soulbound: bool,
    pub random_property_id: u32,
    pub item_text_id: u32,
}

impl AttachedItem {
    pub fn is_empty(&self) -> bool {
        self.entry == 0
    }
}
// Call-pipe completion can precede the Escrow row reaching the subscribed cache.
const ESCROW_VISIBLE_WITHIN: std::time::Duration = std::time::Duration::from_secs(2);
const ESCROW_POLL: std::time::Duration = std::time::Duration::from_millis(20);

fn held_fence<St: MailStore + ?Sized>(
    store: &St,
    sender_guid: u64,
    escrow_id: u64,
) -> Result<HeldEscrow> {
    let deadline = std::time::Instant::now() + ESCROW_VISIBLE_WITHIN;
    loop {
        if let Some(held) = store
            .mail_escrows_of(sender_guid)?
            .into_iter()
            .find(|e| e.escrow_id == escrow_id)
        {
            return Ok(held);
        }
        if std::time::Instant::now() >= deadline {
            return Err(anyhow::anyhow!(
                "mail escrow {escrow_id}: the fence reported success but no row is readable within \
                 {ESCROW_VISIBLE_WITHIN:?}, so the letter's attachment and Delivery Delay cannot be \
                 confirmed"
            ));
        }
        std::thread::sleep(ESCROW_POLL);
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeldEscrow {
    pub escrow_id: u64,
    pub recipient_guid: u64,
    pub subject: String,
    pub body: String,
    pub money: u32,
    pub postage: u32,
    pub payout: bool,
    pub mail_id: u64,
    pub item: AttachedItem,
    pub cod: u32,
    pub delivery_delay_secs: u32,
    pub reward: Option<mail_rules::RewardHeader>,
}
pub(crate) fn redrive<St: MailStore + ShardRoutingStore + ?Sized>(store: &St, self_guid: u64) {
    let Some(actor) = Actor::new(self_guid) else {
        return;
    };
    let realm = match store.realm_store() {
        Ok(realm) => realm,
        Err(error) => {
            log::warn!("mail: recovery for Character {self_guid} waits for Realm-core: {error:#}");
            return;
        }
    };
    let outgoing = store.mail_escrows_of(self_guid).unwrap_or_else(|error| {
        log::warn!("mail: cannot read outgoing Escrow for Character {self_guid}: {error:#}");
        Vec::new()
    });
    for held in outgoing {
        if held.payout {
            continue;
        }
        let outcome = drive(store, held.escrow_id, || match &realm {
            Some(realm) => commit_held(realm.as_ref(), actor, &held),
            None => commit_held(store, actor, &held),
        });
        let kind = if held.reward.is_some() {
            "Reward Letter"
        } else {
            "send"
        };
        log_redrive(kind, held.escrow_id, outcome);
    }
    let Some(realm) = realm else {
        return;
    };
    let payouts = realm.mail_escrows_of(self_guid).unwrap_or_else(|error| {
        log::warn!("mail: cannot read payout Escrow for Character {self_guid}: {error:#}");
        Vec::new()
    });
    for held in payouts {
        if !held.payout {
            continue;
        }
        let outcome = drive(realm.as_ref(), held.escrow_id, || {
            if held.item.is_empty() {
                store.mail_payout(held.escrow_id, actor, held.mail_id, held.money)
            } else {
                store.mail_item_payout(held.escrow_id, actor, held.mail_id, held.item.clone())
            }
        });
        log_redrive("take", held.escrow_id, outcome);
    }
}

fn commit_held<P: MailStore + ?Sized>(plane: &P, sender: Actor, held: &HeldEscrow) -> Result<()> {
    plane.mail_commit(
        held.escrow_id,
        sender,
        held.recipient_guid,
        held.subject.clone(),
        held.body.clone(),
        held.money,
        held.item.clone(),
        held.cod,
        held.mail_id,
        held.delivery_delay_secs,
        held.reward,
    )
}

fn log_redrive(kind: &str, escrow_id: u64, outcome: Result<()>) {
    match outcome {
        Ok(()) => log::info!("mail escrow {escrow_id}: held {kind} driven to completion"),
        Err(e) => log::warn!(
            "mail escrow {escrow_id}: {kind} re-drive failed, the fence is still HELD: {e:#}"
        ),
    }
}
pub(crate) fn take_money<St: MailStore + ShardRoutingStore + ?Sized>(
    store: &St,
    actor: Option<Actor>,
    mailbox_guid: u64,
    mail_id: u64,
) -> MailResult<()> {
    let actor = at_mailbox(store, actor, mailbox_guid)?;
    let Some(realm) = store.realm_store()? else {
        return Ok(store.mail_take_money(actor, mail_id)?);
    };
    let amount = mail_of(store, actor.guid())?
        .into_iter()
        .find(|m| m.id == mail_id)
        .map(|m| m.money)
        .ok_or_else(|| refused(mail_rules::NOT_YOUR_MAIL))?;
    if amount == 0 {
        return Err(refused(mail_rules::NOTHING_TO_TAKE));
    }
    let escrow_id = next_escrow_id()?;
    realm.mail_take_money_fence(escrow_id, actor, mail_id, amount)?;
    drive(realm.as_ref(), escrow_id, || {
        store.mail_payout(escrow_id, actor, mail_id, amount)
    })?;
    Ok(())
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TakeItemRefusal {
    BagsFull(String),
    CannotAffordCod(String),
    Other(String),
}

impl std::fmt::Display for TakeItemRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BagsFull(e) | Self::CannotAffordCod(e) | Self::Other(e) => f.write_str(e),
        }
    }
}

impl FromReason for TakeItemRefusal {
    fn from_reason(reason: &str) -> Self {
        let text = reason.to_string();
        if text.contains(mail_rules::INVENTORY_FULL) {
            TakeItemRefusal::BagsFull(text)
        } else if text.contains(mail_rules::COD_NOT_AFFORDABLE)
            || text.contains(mail_rules::NOT_ENOUGH_MONEY)
        {
            TakeItemRefusal::CannotAffordCod(text)
        } else {
            TakeItemRefusal::Other(text)
        }
    }
}
pub(crate) fn take_item<St: MailStore + ShardRoutingStore + ?Sized>(
    store: &St,
    actor: Option<Actor>,
    mailbox_guid: u64,
    mail_id: u64,
) -> MailResult<(u32, u32), TakeItemRefusal> {
    let actor = at_mailbox(store, actor, mailbox_guid)?;
    let row = mail_of(store, actor.guid())?
        .into_iter()
        .find(|m| m.id == mail_id)
        .ok_or_else(|| refused(mail_rules::NOT_YOUR_MAIL))?;
    if row.item_entry == 0 {
        return Err(refused(mail_rules::NOTHING_TO_TAKE));
    }
    let taken = (row.item_entry, row.item_stack_count);
    let Some(realm) = store.realm_store()? else {
        store.mail_take_item(actor, mail_id)?;
        return Ok(taken);
    };
    store.mail_item_room(actor)?;
    pay_cod(store, realm.as_ref(), actor, &row)?;
    let escrow_id = next_escrow_id()?;
    realm.mail_take_item_fence(escrow_id, actor, mail_id, row.item_entry)?;
    let item = held_fence(realm.as_ref(), actor.guid(), escrow_id)?.item;
    drive(realm.as_ref(), escrow_id, || {
        store.mail_item_payout(escrow_id, actor, mail_id, item)
    })?;
    Ok(taken)
}
fn pay_cod<St: MailStore + ?Sized>(
    store: &St,
    realm: &dyn WorldStore,
    taker: Actor,
    row: &MailView,
) -> Result<()> {
    let taker_guid = taker.guid();
    let Some(settlement) =
        mail_rules::cod_settlement(row.cod, row.sender_guid, &row.subject, taker_guid)
    else {
        return Ok(());
    };
    let escrow_id = store
        .mail_escrows_of(taker_guid)
        .unwrap_or_default()
        .into_iter()
        .find(|e| !e.payout && e.mail_id == row.id)
        .map(|e| e.escrow_id)
        .map(Ok)
        .unwrap_or_else(next_escrow_id)?;
    store.mail_fence(
        escrow_id,
        taker,
        settlement.payee_guid,
        settlement.subject.clone(),
        String::new(),
        settlement.copper,
        0,
        0,
        0,
        row.id,
        false,
    )?;
    drive(store, escrow_id, || {
        realm.mail_commit(
            escrow_id,
            taker,
            settlement.payee_guid,
            settlement.subject.clone(),
            String::new(),
            settlement.copper,
            AttachedItem::default(),
            0,
            row.id,
            NO_DELIVERY_DELAY,
            None,
        )
    })
}
fn next_escrow_id() -> Result<u64> {
    use std::sync::atomic::Ordering;
    #[cfg(test)]
    if ESCROW_ID_RANGE.get().is_none() {
        install_escrow_id_range(1, u64::MAX)?;
    }
    let range = ESCROW_ID_RANGE.get().ok_or_else(|| {
        anyhow::anyhow!(
        "mail escrow id range was not claimed; refusing instead of falling back to colliding ids"
    )
    })?;
    let id = range.next.fetch_add(1, Ordering::Relaxed);
    if id >= range.end {
        anyhow::bail!("mail escrow id range is exhausted")
    }
    Ok(id)
}
pub(crate) fn install_escrow_id_range(next: u64, end: u64) -> Result<()> {
    use std::sync::atomic::AtomicU64;
    let requested_start = next.max(1);
    let range = ESCROW_ID_RANGE.get_or_init(|| EscrowIdRange {
        next: AtomicU64::new(requested_start),
        start: requested_start,
        end,
    });
    if range.start != requested_start || range.end != end {
        anyhow::bail!("a different mail escrow id range is already installed")
    }
    Ok(())
}
fn at_mailbox<St: MailStore + ?Sized, R: FromReason>(
    store: &St,
    actor: Option<Actor>,
    mailbox_guid: u64,
) -> MailResult<Actor, R> {
    let actor = actor.ok_or_else(|| refused(mail_rules::NOT_IN_WORLD))?;
    if !store.mailbox_in_range(mailbox_guid, actor.guid())? {
        return Err(refused(&mail_rules::not_at_mailbox(mailbox_guid)));
    }
    Ok(actor)
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CopyLetterRefusal {
    NoMailbox(String),
    BagsFull(String),
    Other(String),
}

impl std::fmt::Display for CopyLetterRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoMailbox(e) | Self::BagsFull(e) | Self::Other(e) => f.write_str(e),
        }
    }
}

impl FromReason for CopyLetterRefusal {
    fn from_reason(reason: &str) -> Self {
        let text = reason.to_string();
        if text.contains(mail_rules::INVENTORY_FULL) {
            CopyLetterRefusal::BagsFull(text)
        } else {
            CopyLetterRefusal::Other(text)
        }
    }
}
pub(crate) fn copy_letter<St: MailStore + ShardRoutingStore + ?Sized>(
    store: &St,
    actor: Option<Actor>,
    mailbox_guid: u64,
    mail_id: u64,
) -> MailResult<(), CopyLetterRefusal> {
    let actor = at_mailbox::<St, String>(store, actor, mailbox_guid)
        .map_err(|failure| failure.map(CopyLetterRefusal::NoMailbox))?;
    store.mail_item_room(actor)?;
    match store.realm_store()? {
        Some(realm) => realm.mail_copy_text(actor, mail_id),
        None => store.mail_copy_text(actor, mail_id),
    }?;
    let item_text_id = u32::try_from(mail_id).unwrap_or(0);
    store.mail_grant_letter(actor, item_text_id)?;
    match store.realm_store()? {
        Some(realm) => realm.mail_mark_letter_granted(actor, mail_id),
        None => store.mail_mark_letter_granted(actor, mail_id),
    }?;
    Ok(())
}
pub(crate) fn item_text<St: MailStore + ShardRoutingStore + ?Sized>(
    store: &St,
    actor: Option<Actor>,
    item_text_id: u32,
    hint_item_guid: u64,
) -> MailResult<Option<String>> {
    let own_mail_body = letter_body(store, actor, u64::from(item_text_id))?;
    let owns_item = if own_mail_body.is_some() {
        false
    } else {
        match actor {
            Some(actor) => store.owns_item_with_text(actor.guid(), item_text_id, hint_item_guid)?,
            None => false,
        }
    };
    if !owns_item && own_mail_body.is_none() {
        return Ok(None);
    }
    let copied = match store.realm_store()? {
        Some(realm) => realm.item_text(item_text_id),
        None => store.item_text(item_text_id),
    }?;
    Ok(copied.or(own_mail_body))
}
