//! Durable imported auction houses, value-preserving bid and Cancellation transport, and atomic
//! settlement.

use spacetimedb::{reducer, table, ReducerContext, ScheduleAt, Table, Timestamp};

use lyracore_shared::auction::bid_outcome::{
    ACCEPTED as BID_ACCEPTED, BID_INCREMENT, BID_OWN, CANCELLED as BID_CANCELLED,
    DATABASE as BID_DATABASE, HIGHER_BID as BID_HIGHER, ITEM_NOT_FOUND as BID_ITEM_NOT_FOUND,
    PENDING as BID_PENDING,
};
use lyracore_shared::auction::{
    auction_cut, auction_notice, bid_increment, hold_operation, AuctionRefusal,
};
use lyracore_shared::mail::{MailSender, CHECK_MASK_COPIED};

use crate::import_meta::game_import_meta;
use crate::mail::game_mail;
use crate::{game_faction_template, game_item_instance, game_item_template, game_world_entity};

/// Client-authored auction-house policy imported from `AuctionHouse.dbc`.
#[table(accessor = game_auction_house, public)]
pub struct AuctionHouseDefinition {
    #[primary_key]
    pub id: u32,
    pub faction: u32,
    pub deposit_rate: u32,
    pub consignment_rate: u32,
    pub name: String,
}

/// One active listing in its imported house market. The complete item-instance snapshot is the
/// item while it is listed; no inventory row exists until ordinary mail returns or delivers it.
#[table(
    accessor = game_auction,
    public,
    index(accessor = by_owner, btree(columns = [owner_guid])),
    index(accessor = by_highest_bidder, btree(columns = [highest_bidder_guid]))
)]
pub struct Auction {
    #[primary_key]
    #[auto_inc]
    pub id: u32,
    pub listing_operation_id: u64,
    pub house: u32,
    pub owner_guid: u64,
    pub item_guid: u64,
    pub item_entry: u32,
    pub item_stack_count: u32,
    pub item_durability: u32,
    pub item_enchant_id: u32,
    pub item_soulbound: bool,
    pub start_bid: u32,
    pub buyout: u32,
    pub highest_bidder_guid: u64,
    pub highest_bid: u32,
    pub deposit: u32,
    pub created_at: Timestamp,
    pub expires_at: Timestamp,
    pub revision: u64,
    pub deposit_rate: u32,
    pub consignment_rate: u32,
    #[default(0)]
    pub random_property_id: u32,
    /// The listed Plain Letter's `ITEM_FIELD_ITEM_TEXT_ID`. 0 for every other item.
    #[default(0u32)]
    pub item_text_id: u32,
}

/// Source-shard value reserved by a sharded listing operation. An active operation receipt makes
/// the row recovery evidence rather than spendable value; it is deleted only after that evidence
/// is durably copied back to the source. A refused listing instead keeps this Hold until
/// Realm-core durably commits its refund Mail and zero-id receipt.
#[table(
    accessor = game_auction_hold,
    index(accessor = by_seller, btree(columns = [seller_guid]))
)]
pub struct AuctionHold {
    #[primary_key]
    pub operation_id: u64,
    pub seller_guid: u64,
    pub item_guid: u64,
    pub item_entry: u32,
    pub item_stack_count: u32,
    pub item_durability: u32,
    pub item_enchant_id: u32,
    pub item_soulbound: bool,
    pub start_bid: u32,
    pub buyout: u32,
    pub duration_minutes: u32,
    pub deposit: u32,
    pub created_micros: i64,
    pub expires_micros: i64,
    pub house: u32,
    pub deposit_rate: u32,
    pub consignment_rate: u32,
    #[default(0)]
    pub random_property_id: u32,
    /// The listed Plain Letter's `ITEM_FIELD_ITEM_TEXT_ID`. 0 for every other item.
    #[default(0u32)]
    pub item_text_id: u32,
}

// A listing Hold keeps the item and the deposit on the seller's Home Shard, and its refund mails
// them back from that Shard. It travels with its Character like the bid Hold, so the next
// MSG_AUCTION_HELLO on the new Home Shard finishes the listing. This sweep deletes only a Hold that
// travelled: a Transfer's cascade keeps every Hold its payload does not carry
// (`transfer::ListingHolds::Keep`). That is the destination's own Hold on an import, and the
// source's Hold under a blob from the build before Holds travelled. A kept Hold finishes on the
// next MSG_AUCTION_HELLO its seller sends from that Shard. Character deletion is refused while a
// Hold exists.
crate::character_owned!(delete, fn sweep_delete_game_auction_hold(ctx, character_guid) {
    let operations: Vec<u64> = ctx
.db
.game_auction_hold()
.by_seller()
.filter(&character_guid)
.map(|hold| hold.operation_id)
.collect();
    for operation_id in operations {
        ctx.db.game_auction_hold().operation_id().delete(operation_id);
    }
});
crate::character_owned!(transfer, fn sweep_transfer_game_auction_hold(ctx, character_guid, io) {
    table = game_auction_hold,
    by = by_seller,
    keep_key,
});

/// Durable idempotency receipt. The full listing payload makes identical replay distinguishable
/// from conflicting reuse even after the source Hold has been deleted. Realm-core alone uses
/// `auction_id == 0` as a refused-listing refund receipt; that sentinel never reaches the source.
#[table(
    accessor = game_auction_operation_receipt,
    index(accessor = by_actor, btree(columns = [actor_guid]))
)]
pub struct AuctionOperationReceipt {
    #[primary_key]
    pub operation_id: u64,
    pub auction_id: u32,
    pub actor_guid: u64,
    pub item_guid: u64,
    pub item_entry: u32,
    pub item_stack_count: u32,
    pub item_durability: u32,
    pub item_enchant_id: u32,
    pub item_soulbound: bool,
    pub start_bid: u32,
    pub buyout: u32,
    pub duration_minutes: u32,
    pub deposit: u32,
    pub created_micros: i64,
    pub expires_micros: i64,
    pub house: u32,
    pub deposit_rate: u32,
    pub consignment_rate: u32,
    #[default(0)]
    pub random_property_id: u32,
    /// The listed Plain Letter's `ITEM_FIELD_ITEM_TEXT_ID`. 0 for every other item.
    #[default(0u32)]
    pub item_text_id: u32,
}

/// Source-shard copper Hold for one caller-identified bid or Cancellation. `outcome == 0` is
/// pending; every nonzero outcome is terminal. `accepted_price` records realm-core's normalized
/// charge, while `deferred_refund` retains any remainder that could not fit back in the purse. On a
/// Cancellation (`operation == hold_operation::CANCEL`) `bidder_guid` is the seller and `offer` is
/// the Auction Cut the seller agreed to pay.
#[table(
    accessor = game_auction_bid_hold,
    index(accessor = by_bidder, btree(columns = [bidder_guid]))
)]
pub struct AuctionBidHold {
    #[primary_key]
    pub operation_id: u64,
    pub bidder_guid: u64,
    pub auction_id: u32,
    pub offer: u32,
    pub outcome: u8,
    pub revision: u64,
    pub result_bidder_guid: u64,
    pub result_bid: u32,
    pub minimum_increment: u32,
    #[default(0)]
    pub deferred_refund: u32,
    #[default(0)]
    pub accepted_price: u32,
    pub house: u32,
    #[default(0u8)]
    pub operation: u8,
}

// A Hold is copper on the Character's Home Shard, so it travels with the Character. A Hold left on
// a Shard the Character has left could never be refunded there, because the refund credits the
// purse on the Shard that holds the Hold. Deletion is refused while a Hold is unfinished, so the
// delete sweep only removes finished rows.
crate::character_owned!(delete, fn sweep_delete_game_auction_bid_hold(ctx, character_guid) {
    let operations: Vec<u64> = ctx
.db
.game_auction_bid_hold()
.by_bidder()
.filter(&character_guid)
.map(|hold| hold.operation_id)
.collect();
    for operation_id in operations {
        ctx.db.game_auction_bid_hold().operation_id().delete(operation_id);
    }
});
crate::character_owned!(transfer, fn sweep_transfer_game_auction_bid_hold(ctx, character_guid, io) {
    table = game_auction_bid_hold,
    by = by_bidder,
    keep_key,
});

/// Realm-core's terminal serialized decision for one bid or Cancellation payload. Auction changes,
/// buyout and Cancellation mail, displaced mail, and any later source-refund mail are exact-once
/// updates recorded on this row.
#[table(accessor = game_auction_bid_decision)]
pub struct AuctionBidDecision {
    #[primary_key]
    pub operation_id: u64,
    pub bidder_guid: u64,
    pub auction_id: u32,
    pub offer: u32,
    pub outcome: u8,
    pub revision: u64,
    pub result_bidder_guid: u64,
    pub result_bid: u32,
    pub minimum_increment: u32,
    #[default(0)]
    pub deferred_refund: u32,
    #[default(0)]
    pub accepted_price: u32,
    pub house: u32,
    /// The auctioned item's catalogue entry and Random Property, captured while the Auction row
    /// still exists. A deferred refund can fire after that row is gone (the auction settled or
    /// expired between the decision and the refund), and this is the only place left to read the
    /// item the refund's Outbid subject names.
    #[default(0u32)]
    pub item_entry: u32,
    #[default(0u32)]
    pub random_property_id: u32,
    #[default(0u8)]
    pub operation: u8,
}

/// One one-shot scheduler row for each active Auction.
#[table(
    accessor = game_auction_expiry,
    scheduled(expire_auction)
)]
pub struct AuctionExpiry {
    #[primary_key]
    #[auto_inc]
    pub scheduled_id: u64,
    pub scheduled_at: ScheduleAt,
    #[unique]
    pub auction_id: u32,
}

/// Private Relay event: a live outbid/won/sold/expired/new-bid/removed notice for one online seller
/// or bidder,
/// inserted in the same transaction as the Auction Mail it accompanies. `kind` is one of
/// `lyracore_shared::auction::auction_notice`. Reaped by the shared event GC, same as
/// `game_whisper_event`.
#[table(
    accessor = game_auction_notice,
    index(accessor = by_recipient, btree(columns = [recipient_guid]))
)]
pub struct AuctionNotice {
    #[primary_key]
    #[auto_inc]
    pub id: u64,
    pub recipient_guid: u64,
    pub kind: u8,
    pub house: u32,
    pub auction_id: u32,
    pub item_entry: u32,
    pub random_property_id: u32,
    pub bid: u32,
    pub out_bid: u32,
    pub bidder_guid: u64,
    pub created_at: Timestamp,
}

// Auction durability belongs to the listing protocol, not character transport. Active Auction or
// Hold value blocks character deletion, and every row except the two Holds stays on the database
// that owns its protocol phase rather than entering the character movement manifest.

// A build 5875 client offers only three listing durations, 2, 8 and 24 hours
// (`AuctionsRadioButton_OnClick`, fx:Blizzard_AuctionUI.lua:991-1006; the default selection at
// line 798 picks the 8-hour middle button). cmangos-classic accepts exactly the matching etime,
// 1, 4 or 12 times its `MIN_AUCTION_TIME` of 2 hours (cm:AuctionHouseHandler.cpp:250-262,
// cm:AuctionHouseMgr.h:30), and the deposit scales by that same multiplier:
// `SellPrice * count * (etime / MIN_AUCTION_TIME) * depositPercent / 100`
// (cm:AuctionHouseMgr.cpp:65-77, `GetAuctionDeposit`).
fn duration_multiplier(duration_minutes: u32) -> Option<u64> {
    match duration_minutes {
        120 => Some(1),
        480 => Some(4),
        1_440 => Some(12),
        _ => None,
    }
}

fn valid_rate(rate: u32) -> bool {
    rate <= 100
}

fn listing_deposit(
    sell_price: u32,
    stack_count: u32,
    duration_minutes: u32,
    deposit_rate: u32,
) -> Option<u32> {
    if !valid_rate(deposit_rate) {
        return None;
    }
    let multiplier = duration_multiplier(duration_minutes)?;
    let deposit = u64::from(sell_price)
        .checked_mul(u64::from(stack_count))?
        .checked_mul(u64::from(deposit_rate))?
        .checked_mul(multiplier)?
        / 100;
    u32::try_from(deposit.max(1)).ok()
}

fn seller_proceeds(winning_price: u32, deposit: u32, consignment_rate: u32) -> Option<u32> {
    let cut = auction_cut(winning_price, consignment_rate)?;
    let after_cut = winning_price.checked_sub(cut)?;
    after_cut.checked_add(deposit)
}

fn listing_proceeds_are_representable(
    terms: ListingTerms,
    deposit: u32,
    consignment_rate: u32,
) -> bool {
    seller_proceeds(terms.start_bid, deposit, consignment_rate).is_some()
        && (terms.buyout == 0 || seller_proceeds(terms.buyout, deposit, consignment_rate).is_some())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AuctionHousePolicy {
    id: u32,
    deposit_rate: u32,
    consignment_rate: u32,
}

fn imported_house_policy(ctx: &ReducerContext, house: u32) -> Result<AuctionHousePolicy, String> {
    let row = ctx
        .db
        .game_auction_house()
        .id()
        .find(house)
        .ok_or_else(|| {
            refused(
                AuctionRefusal::InvalidTerms,
                "auction house is not imported",
            )
        })?;
    if row.id == 0 || !valid_rate(row.deposit_rate) || !valid_rate(row.consignment_rate) {
        return Err(refused(
            AuctionRefusal::InvalidTerms,
            "imported auction house policy is invalid",
        ));
    }
    Ok(AuctionHousePolicy {
        id: row.id,
        deposit_rate: row.deposit_rate,
        consignment_rate: row.consignment_rate,
    })
}

fn auction_house_for_interaction(
    ctx: &ReducerContext,
    player_guid: u64,
    auctioneer_guid: u64,
) -> Option<AuctionHousePolicy> {
    let player = crate::helpers::acting_entity_by_guid(ctx, player_guid)?;
    let auctioneer = ctx.db.game_world_entity().guid().find(auctioneer_guid)?;
    if !player.is_player()
        || player.dead
        || player.health == 0
        || auctioneer.dead
        || auctioneer.health == 0
        || auctioneer.is_player()
        || auctioneer.type_mask & lyracore_shared::constants::type_mask::CREATURE
            != lyracore_shared::constants::type_mask::CREATURE
        || auctioneer.npc_flags & lyracore_shared::constants::npc_flags::AUCTIONEER == 0
        || auctioneer.unit_flags & lyracore_shared::constants::unit_flags::NOT_SELECTABLE != 0
        || player.map_id != auctioneer.map_id
        || player.instance_id != auctioneer.instance_id
        || crate::helpers::dist_sq(&player, &auctioneer)
            > lyracore_shared::auction::INTERACTION_RANGE_SQ
        || crate::reputation::npc_refuses_interaction(ctx, &auctioneer, &player)
    {
        return None;
    }
    let faction_group = ctx
        .db
        .game_faction_template()
        .id()
        .find(auctioneer.faction_template)?
        .faction_group;
    let house_id = lyracore_shared::auction::house_for_faction_template(
        auctioneer.faction_template,
        faction_group,
    );
    ctx.db
        .game_auction_house()
        .id()
        .find(house_id)
        .filter(|house| {
            house.id != 0 && valid_rate(house.deposit_rate) && valid_rate(house.consignment_rate)
        })
        .map(|house| AuctionHousePolicy {
            id: house.id,
            deposit_rate: house.deposit_rate,
            consignment_rate: house.consignment_rate,
        })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ListingItem {
    guid: u64,
    owner_guid: u64,
    slot: u8,
    mailable: bool,
    snapshot: crate::items::ItemSnapshot,
    sell_price: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ListingTerms {
    start_bid: u32,
    buyout: u32,
    duration_minutes: u32,
}

const FIRST_BACKPACK_SLOT: u8 = 23;
const MICROS_PER_MINUTE: i64 = 60_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ListingRequest {
    operation_id: u64,
    seller_guid: u64,
    item_guid: u64,
    house: AuctionHousePolicy,
    terms: ListingTerms,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PreparedListing {
    request: ListingRequest,
    snapshot: crate::items::ItemSnapshot,
    deposit: u32,
    created_micros: i64,
    expires_micros: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ListingReceipt {
    listing: PreparedListing,
    auction_id: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ListingHold {
    listing: PreparedListing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ListingRefund {
    listing: PreparedListing,
    mail: AuctionMail,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OperationMatch {
    Replay(u32),
    Conflict,
    Fresh,
}

/// What a bid Hold row pays for. Stored as `hold_operation` codes by position, so only append.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HoldOperation {
    Bid,
    Cancel,
}

impl HoldOperation {
    fn code(self) -> u8 {
        match self {
            Self::Bid => hold_operation::BID,
            Self::Cancel => hold_operation::CANCEL,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            hold_operation::BID => Some(Self::Bid),
            hold_operation::CANCEL => Some(Self::Cancel),
            _ => None,
        }
    }
}

/// One Hold's identity. On a Cancel, `bidder_guid` is the seller and `offer` is the Auction Cut
/// the Gateway read, which Realm-core must still find current.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HoldRequest {
    operation: HoldOperation,
    operation_id: u64,
    bidder_guid: u64,
    auction_id: u32,
    house: u32,
    offer: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BidAuction {
    id: u32,
    house: u32,
    owner_guid: u64,
    item: crate::items::ItemSnapshot,
    highest_bidder_guid: u64,
    highest_bid: u32,
    start_bid: u32,
    buyout: u32,
    deposit: u32,
    consignment_rate: u32,
    expires_micros: i64,
    revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuctionBidEffect {
    RemainActive { revision: u64 },
    SettleBuyout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BidAcceptance {
    price: u32,
    effect: AuctionBidEffect,
    displaced_bidder_guid: u64,
    displaced_bid: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HoldDecision {
    Accepted(BidAcceptance),
    ItemNotFound,
    HigherBid {
        bidder_guid: u64,
        current_bid: u32,
        minimum_increment: u32,
    },
    BidIncrement,
    BidOwn,
    Database,
    /// The seller withdrew the listing and pays `cut`. A displaced bidder gets `displaced_bid`
    /// back by mail.
    Cancelled {
        cut: u32,
        displaced_bidder_guid: u64,
        displaced_bid: u32,
    },
}

impl HoldDecision {
    /// Whether Realm-core can reach this decision for `operation`. A decision for the other
    /// operation is a forged or crossed payload.
    fn belongs_to(self, operation: HoldOperation) -> bool {
        match self {
            Self::Accepted(_) | Self::HigherBid { .. } | Self::BidIncrement | Self::BidOwn => {
                operation == HoldOperation::Bid
            }
            Self::Cancelled { .. } => operation == HoldOperation::Cancel,
            Self::ItemNotFound | Self::Database => true,
        }
    }
}

/// The vanilla `MailAuctionAnswers` action code embedded in an Auction Mail's subject
/// (`cm:Mail.h:100-109`). `Cancelled` (5) returns a cancelled listing's item to its seller; a
/// refused listing's return uses it too, as the closest client string for a return with no vanilla
/// twin. `CancelledToBidder` (4) refunds the displaced bid of a cancelled listing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuctionMailAction {
    Outbid = 0,
    Won = 1,
    Successful = 2,
    Expired = 3,
    CancelledToBidder = 4,
    Cancelled = 5,
}

/// An Auction Mail's own vanilla shape: `MAIL_AUCTION` from the house, `checked = COPIED`
/// (`cm:Mail.cpp:81-84`), a machine subject the client turns into `AUCTION_*_MAIL_SUBJECT`
/// (`fx:GlobalStrings.lua:83,87,88,89,99`), and — for Won/Successful — an invoice body the client
/// turns into the auction details (`fx:MailFrame.lua:300-361`). `item_entry`/`random_property_id`
/// name the AUCTIONED item, which the subject always carries even when nothing is attached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AuctionMail {
    recipient_guid: u64,
    house: u32,
    action: AuctionMailAction,
    item_entry: u32,
    random_property_id: u32,
    money: u32,
    attached_item: crate::items::ItemSnapshot,
    /// The invoice's other party: the seller on a Won mail, the winning bidder on a Successful
    /// mail. Unused (0) on every other action.
    counterparty_guid: u64,
    bid: u32,
    buyout: u32,
    deposit: u32,
    cut: u32,
}

fn displaced_bid_refund_mail(
    house: u32,
    item: crate::items::ItemSnapshot,
    bidder_guid: u64,
    bid: u32,
) -> Option<AuctionMail> {
    (bidder_guid != 0).then_some(AuctionMail {
        recipient_guid: bidder_guid,
        house,
        action: AuctionMailAction::Outbid,
        item_entry: item.entry,
        random_property_id: item.random_property_id,
        money: bid,
        attached_item: crate::items::ItemSnapshot::default(),
        counterparty_guid: 0,
        bid: 0,
        buyout: 0,
        deposit: 0,
        cut: 0,
    })
}

#[allow(clippy::too_many_arguments)]
fn sale_settlement_mail(
    house: u32,
    owner_guid: u64,
    item: crate::items::ItemSnapshot,
    winner_guid: u64,
    winning_price: u32,
    buyout: u32,
    deposit: u32,
    consignment_rate: u32,
) -> Option<[AuctionMail; 2]> {
    let cut = auction_cut(winning_price, consignment_rate)?;
    let proceeds = seller_proceeds(winning_price, deposit, consignment_rate)?;
    Some([
        AuctionMail {
            recipient_guid: winner_guid,
            house,
            action: AuctionMailAction::Won,
            item_entry: item.entry,
            random_property_id: item.random_property_id,
            money: 0,
            attached_item: item,
            counterparty_guid: owner_guid,
            bid: winning_price,
            buyout,
            deposit: 0,
            cut: 0,
        },
        AuctionMail {
            recipient_guid: owner_guid,
            house,
            action: AuctionMailAction::Successful,
            item_entry: item.entry,
            random_property_id: item.random_property_id,
            money: proceeds,
            attached_item: crate::items::ItemSnapshot::default(),
            counterparty_guid: winner_guid,
            bid: winning_price,
            buyout,
            deposit,
            cut,
        },
    ])
}

fn buyout_settlement_mail(
    auction: BidAuction,
    winner_guid: u64,
    price: u32,
) -> Option<[AuctionMail; 2]> {
    if auction.buyout == 0 || price != auction.buyout {
        return None;
    }
    sale_settlement_mail(
        auction.house,
        auction.owner_guid,
        auction.item,
        winner_guid,
        price,
        auction.buyout,
        auction.deposit,
        auction.consignment_rate,
    )
}

/// Mail that returns a refused listing's item and deposit to the seller. No vanilla auction
/// refuses a listing after acceptance, so this renders as the closest client string, a Cancelled
/// mail carrying the item and the deposit, and hides Return the same way every other Auction Mail
/// does.
fn listing_release_mail(listing: &PreparedListing) -> AuctionMail {
    AuctionMail {
        recipient_guid: listing.request.seller_guid,
        house: listing.request.house.id,
        action: AuctionMailAction::Cancelled,
        item_entry: listing.snapshot.entry,
        random_property_id: listing.snapshot.random_property_id,
        money: listing.deposit,
        attached_item: listing.snapshot,
        counterparty_guid: 0,
        bid: 0,
        buyout: 0,
        deposit: 0,
        cut: 0,
    }
}

/// A Cancellation's mail (`cm:AuctionHouseHandler.cpp:167-189,440-456`): the item goes back to the
/// seller in a Cancelled mail, and a displaced bidder gets the bid back in a Cancelled-to-bidder
/// mail. No mail carries the deposit, because the house keeps it.
fn cancellation_mail(
    auction: BidAuction,
    displaced_bidder_guid: u64,
    displaced_bid: u32,
) -> Vec<AuctionMail> {
    let mail = |recipient_guid, action, money, attached_item| AuctionMail {
        recipient_guid,
        house: auction.house,
        action,
        item_entry: auction.item.entry,
        random_property_id: auction.item.random_property_id,
        money,
        attached_item,
        counterparty_guid: 0,
        bid: 0,
        buyout: 0,
        deposit: 0,
        cut: 0,
    };
    let refund = (displaced_bidder_guid != 0).then(|| {
        mail(
            displaced_bidder_guid,
            AuctionMailAction::CancelledToBidder,
            displaced_bid,
            crate::items::ItemSnapshot::default(),
        )
    });
    refund
        .into_iter()
        .chain([mail(
            auction.owner_guid,
            AuctionMailAction::Cancelled,
            0,
            auction.item,
        )])
        .collect()
}

/// `"{item_entry}:{random_property_id}:{action}"` (`cm:AuctionHouseMgr.cpp:134,181,229`,
/// `cm:AuctionHouseHandler.cpp:155,180,451`). The client builds the visible subject from this and
/// the item's name (`fx:GlobalStrings.lua:83,87,88,89,99`).
fn auction_mail_subject(
    item_entry: u32,
    random_property_id: u32,
    action: AuctionMailAction,
) -> String {
    format!("{item_entry}:{random_property_id}:{}", action as u8)
}

/// The invoice the client renders from a Won or Successful mail's body
/// (`fx:MailFrame.lua:300-361`). The guid is lowercase hex, right-aligned in a 16-character,
/// space-filled field — cmangos prints the low guid, which a 1.12 character guid equals whole.
/// Every other action carries no body.
fn auction_mail_body(mail: &AuctionMail) -> String {
    match mail.action {
        AuctionMailAction::Won => format!(
            "{:>16x}:{}:{}",
            mail.counterparty_guid, mail.bid, mail.buyout
        ),
        AuctionMailAction::Successful => format!(
            "{:>16x}:{}:{}:{}:{}",
            mail.counterparty_guid, mail.bid, mail.buyout, mail.deposit, mail.cut
        ),
        AuctionMailAction::Outbid
        | AuctionMailAction::Expired
        | AuctionMailAction::CancelledToBidder
        | AuctionMailAction::Cancelled => String::new(),
    }
}

fn insert_auction_mail(ctx: &ReducerContext, mail: AuctionMail) {
    crate::mail::insert_letter(
        ctx,
        crate::mail::Letter {
            recipient_guid: mail.recipient_guid,
            sender: MailSender::AuctionHouse(mail.house),
            subject: auction_mail_subject(mail.item_entry, mail.random_property_id, mail.action),
            body: auction_mail_body(&mail),
            money: mail.money,
            cod: 0,
            item: mail.attached_item,
            mail_template_id: 0,
            check_flags: CHECK_MASK_COPIED,
            deliver_micros: 0,
        },
    );
}

/// One `game_auction_notice` row before its id and timestamp are stamped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AuctionNoticeDraft {
    recipient_guid: u64,
    kind: u8,
    house: u32,
    auction_id: u32,
    item_entry: u32,
    random_property_id: u32,
    bid: u32,
    out_bid: u32,
    bidder_guid: u64,
}

fn insert_auction_notice(ctx: &ReducerContext, draft: AuctionNoticeDraft) {
    ctx.db.game_auction_notice().insert(AuctionNotice {
        id: 0,
        recipient_guid: draft.recipient_guid,
        kind: draft.kind,
        house: draft.house,
        auction_id: draft.auction_id,
        item_entry: draft.item_entry,
        random_property_id: draft.random_property_id,
        bid: draft.bid,
        out_bid: draft.out_bid,
        bidder_guid: draft.bidder_guid,
        created_at: ctx.timestamp,
    });
}

/// Won (to the buyer) and Sold (to the seller): the two notices every settled sale sends, whether
/// it settled by buyout or by expiry with a bid (`cm:AuctionHouseMgr.cpp:146-147,195-199`).
#[allow(clippy::too_many_arguments)]
fn settlement_notices(
    house: u32,
    auction_id: u32,
    item_entry: u32,
    random_property_id: u32,
    owner_guid: u64,
    winner_guid: u64,
    winning_price: u32,
) -> [AuctionNoticeDraft; 2] {
    let out_bid = bid_increment(winning_price);
    [
        AuctionNoticeDraft {
            recipient_guid: winner_guid,
            kind: auction_notice::WON,
            house,
            auction_id,
            item_entry,
            random_property_id,
            bid: 0,
            out_bid,
            bidder_guid: winner_guid,
        },
        AuctionNoticeDraft {
            recipient_guid: owner_guid,
            kind: auction_notice::SOLD,
            house,
            auction_id,
            item_entry,
            random_property_id,
            bid: winning_price,
            out_bid,
            bidder_guid: 0,
        },
    ]
}

/// Outbid, to the displaced bidder, sent before the bid changes (`cm:AuctionHouseHandler.cpp:93-107,157-158`).
/// No displaced bidder (a fresh listing's first bid), or the displaced bidder raising their own
/// bid, sends nothing: cmangos's `UpdateBid` charges a self-raise only the delta and never calls
/// `SendAuctionBidderNotification` for it (`cm:AuctionHouseMgr.cpp:780-793`), because the bidder
/// was never actually outbid.
#[allow(clippy::too_many_arguments)]
fn outbid_notice(
    house: u32,
    auction_id: u32,
    item: crate::items::ItemSnapshot,
    displaced_bidder_guid: u64,
    new_bidder_guid: u64,
    displaced_bid: u32,
) -> Option<AuctionNoticeDraft> {
    (displaced_bidder_guid != 0 && displaced_bidder_guid != new_bidder_guid).then_some(
        AuctionNoticeDraft {
            recipient_guid: displaced_bidder_guid,
            kind: auction_notice::OUTBID,
            house,
            auction_id,
            item_entry: item.entry,
            random_property_id: item.random_property_id,
            bid: displaced_bid,
            out_bid: bid_increment(displaced_bid),
            bidder_guid: displaced_bidder_guid,
        },
    )
}

/// New bid, to the owner: the client refreshes its list and prints nothing
/// (`cm:AuctionHouseMgr.cpp:802-806`).
fn new_bid_notice(
    house: u32,
    auction_id: u32,
    item: crate::items::ItemSnapshot,
    owner_guid: u64,
    bidder_guid: u64,
    price: u32,
) -> AuctionNoticeDraft {
    AuctionNoticeDraft {
        recipient_guid: owner_guid,
        kind: auction_notice::NEW_BID,
        house,
        auction_id,
        item_entry: item.entry,
        random_property_id: item.random_property_id,
        bid: price,
        out_bid: bid_increment(price),
        bidder_guid,
    }
}

/// Expired, to the owner: an unsold listing (`cm:AuctionHouseMgr.cpp:231-232`).
fn expired_notice(
    house: u32,
    auction_id: u32,
    item: crate::items::ItemSnapshot,
    owner_guid: u64,
) -> AuctionNoticeDraft {
    AuctionNoticeDraft {
        recipient_guid: owner_guid,
        kind: auction_notice::EXPIRED,
        house,
        auction_id,
        item_entry: item.entry,
        random_property_id: item.random_property_id,
        bid: 0,
        // `bid_increment` returns 0 for a zero bid — nothing to raise — but the client always
        // shows a nonzero minimum raise, even on an unsold listing. Floor it the same way cmangos
        // does (`cm:AuctionHouseMgr.cpp:739-745`: `if (!outbid) outbid = 1`).
        out_bid: 1,
        bidder_guid: 0,
    }
}

/// Removed, to the bidder a Cancellation displaces (`cm:AuctionHouseHandler.cpp:131-139,182-183`).
/// The client prints `ERR_AUCTION_REMOVED_S` with the item's name.
fn removed_notice(
    house: u32,
    auction_id: u32,
    item: crate::items::ItemSnapshot,
    displaced_bidder_guid: u64,
    displaced_bid: u32,
) -> Option<AuctionNoticeDraft> {
    (displaced_bidder_guid != 0).then_some(AuctionNoticeDraft {
        recipient_guid: displaced_bidder_guid,
        kind: auction_notice::REMOVED,
        house,
        auction_id,
        item_entry: item.entry,
        random_property_id: item.random_property_id,
        bid: displaced_bid,
        out_bid: 0,
        bidder_guid: displaced_bidder_guid,
    })
}

#[derive(Clone, Copy)]
struct BidDecisionFields {
    outcome: u8,
    revision: u64,
    result_bidder_guid: u64,
    result_bid: u32,
    minimum_increment: u32,
    accepted_price: u32,
}

fn bid_decision_fields(decision: HoldDecision) -> BidDecisionFields {
    let mut fields = BidDecisionFields {
        outcome: BID_DATABASE,
        revision: 0,
        result_bidder_guid: 0,
        result_bid: 0,
        minimum_increment: 0,
        accepted_price: 0,
    };
    match decision {
        HoldDecision::Accepted(BidAcceptance {
            price,
            effect,
            displaced_bidder_guid,
            displaced_bid,
        }) => {
            fields.outcome = BID_ACCEPTED;
            fields.revision = match effect {
                AuctionBidEffect::RemainActive { revision } => revision,
                AuctionBidEffect::SettleBuyout => 0,
            };
            fields.result_bidder_guid = displaced_bidder_guid;
            fields.result_bid = displaced_bid;
            fields.accepted_price = price;
        }
        HoldDecision::ItemNotFound => fields.outcome = BID_ITEM_NOT_FOUND,
        HoldDecision::HigherBid {
            bidder_guid,
            current_bid,
            minimum_increment,
        } => {
            fields.outcome = BID_HIGHER;
            fields.result_bidder_guid = bidder_guid;
            fields.result_bid = current_bid;
            fields.minimum_increment = minimum_increment;
        }
        HoldDecision::BidIncrement => fields.outcome = BID_INCREMENT,
        HoldDecision::BidOwn => fields.outcome = BID_OWN,
        HoldDecision::Database => {}
        HoldDecision::Cancelled {
            cut,
            displaced_bidder_guid,
            displaced_bid,
        } => {
            fields.outcome = BID_CANCELLED;
            fields.result_bidder_guid = displaced_bidder_guid;
            fields.result_bid = displaced_bid;
            fields.accepted_price = cut;
        }
    }
    fields
}

fn bid_decision_from_fields(fields: BidDecisionFields, legacy_offer: u32) -> Option<HoldDecision> {
    match fields.outcome {
        BID_PENDING => None,
        BID_ACCEPTED => Some(HoldDecision::Accepted(BidAcceptance {
            price: if fields.accepted_price == 0 {
                legacy_offer
            } else {
                fields.accepted_price
            },
            effect: if fields.revision == 0 {
                AuctionBidEffect::SettleBuyout
            } else {
                AuctionBidEffect::RemainActive {
                    revision: fields.revision,
                }
            },
            displaced_bidder_guid: fields.result_bidder_guid,
            displaced_bid: fields.result_bid,
        })),
        BID_ITEM_NOT_FOUND => Some(HoldDecision::ItemNotFound),
        BID_HIGHER => Some(HoldDecision::HigherBid {
            bidder_guid: fields.result_bidder_guid,
            current_bid: fields.result_bid,
            minimum_increment: fields.minimum_increment,
        }),
        BID_INCREMENT => Some(HoldDecision::BidIncrement),
        BID_OWN => Some(HoldDecision::BidOwn),
        // No legacy fallback: a Cancellation's cut of 0 is a real charge of nothing.
        BID_CANCELLED => Some(HoldDecision::Cancelled {
            cut: fields.accepted_price,
            displaced_bidder_guid: fields.result_bidder_guid,
            displaced_bid: fields.result_bid,
        }),
        _ => Some(HoldDecision::Database),
    }
}

fn held_bid_decision(row: &AuctionBidHold) -> Option<HoldDecision> {
    bid_decision_from_fields(
        BidDecisionFields {
            outcome: row.outcome,
            revision: row.revision,
            result_bidder_guid: row.result_bidder_guid,
            result_bid: row.result_bid,
            minimum_increment: row.minimum_increment,
            accepted_price: row.accepted_price,
        },
        row.offer,
    )
}

fn realm_bid_decision(row: &AuctionBidDecision) -> Option<HoldDecision> {
    bid_decision_from_fields(
        BidDecisionFields {
            outcome: row.outcome,
            revision: row.revision,
            result_bidder_guid: row.result_bidder_guid,
            result_bid: row.result_bid,
            minimum_increment: row.minimum_increment,
            accepted_price: row.accepted_price,
        },
        row.offer,
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HeldBid {
    request: HoldRequest,
    decision: Option<HoldDecision>,
    deferred_refund: u32,
}

trait BidSource {
    fn money(&self, bidder_guid: u64) -> Option<u32>;
    fn hold(&self, operation_id: u64) -> Option<HeldBid>;
    fn create_hold(&mut self, request: HoldRequest) -> Result<(), AuctionRefusal>;
    fn finish_hold(
        &mut self,
        request: HoldRequest,
        decision: HoldDecision,
    ) -> Result<(), AuctionRefusal>;
    fn confirm_refund(&mut self, request: HoldRequest) -> Result<(), AuctionRefusal>;
}

/// A request no Hold can carry: a zero operation, Character, listing or house, or a bid of 0. A
/// Cancellation's cut is 0 for a listing nobody bid on.
fn request_is_malformed(request: &HoldRequest) -> bool {
    request.operation_id == 0
        || request.bidder_guid == 0
        || request.auction_id == 0
        || request.house == 0
        || (request.operation == HoldOperation::Bid && request.offer == 0)
}

/// The listing `request` names, while it is still active in the request's Auction Market.
fn active_listing(
    auction: Option<BidAuction>,
    request: &HoldRequest,
    now_micros: i64,
) -> Option<BidAuction> {
    auction.filter(|auction| {
        auction.id == request.auction_id
            && lyracore_shared::auction::market_of(auction.house)
                == lyracore_shared::auction::market_of(request.house)
            && auction.expires_micros > now_micros
    })
}

/// Move a Hold's value out of the purse. A bid holds its full offer. A Cancellation holds the Auction
/// Cut, which is 0 for a listing nobody bid on, and still takes the Hold so the interaction Gate
/// runs on the Home Shard.
fn fence_bid<S: BidSource>(source: &mut S, request: HoldRequest) -> Result<(), AuctionRefusal> {
    if request_is_malformed(&request) {
        return Err(AuctionRefusal::Database);
    }
    if let Some(hold) = source.hold(request.operation_id) {
        return if hold.request == request {
            Ok(())
        } else {
            Err(AuctionRefusal::Database)
        };
    }
    if source
        .money(request.bidder_guid)
        .is_none_or(|money| money < request.offer)
    {
        return Err(AuctionRefusal::NotEnoughMoney);
    }
    source.create_hold(request)?;
    Ok(())
}

fn finish_bid<S: BidSource>(
    source: &mut S,
    request: HoldRequest,
    decision: HoldDecision,
) -> Result<HoldDecision, AuctionRefusal> {
    let hold = source
        .hold(request.operation_id)
        .ok_or(AuctionRefusal::Database)?;
    if hold.request != request || !decision.belongs_to(request.operation) {
        return Err(AuctionRefusal::Database);
    }
    if let Some(existing) = hold.decision {
        return if existing == decision {
            Ok(existing)
        } else {
            Err(AuctionRefusal::Database)
        };
    }
    source.finish_hold(request, decision)?;
    Ok(decision)
}

fn refundable_bid_value(decision: HoldDecision, offer: u32) -> Option<u32> {
    match decision {
        HoldDecision::Accepted(BidAcceptance { price, .. }) if price != 0 && price <= offer => {
            offer.checked_sub(price)
        }
        HoldDecision::Accepted(_) => None,
        // A Cancellation spends exactly the cut it held; a different cut was never agreed.
        HoldDecision::Cancelled { cut, .. } if cut == offer => Some(0),
        HoldDecision::Cancelled { .. } => None,
        _ => Some(offer),
    }
}

fn minimum_next_bid(auction: BidAuction) -> Result<u32, HoldDecision> {
    lyracore_shared::auction::minimum_next_bid(auction.start_bid, auction.highest_bid)
        .ok_or(HoldDecision::Database)
}

fn decide_bid(auction: Option<BidAuction>, request: HoldRequest, now_micros: i64) -> HoldDecision {
    if request.operation != HoldOperation::Bid || request_is_malformed(&request) {
        return HoldDecision::Database;
    }
    let Some(auction) = active_listing(auction, &request, now_micros) else {
        return HoldDecision::ItemNotFound;
    };
    if auction.owner_guid == request.bidder_guid {
        return HoldDecision::BidOwn;
    }
    let is_buyout = auction.buyout != 0 && request.offer >= auction.buyout;
    let minimum_increment = lyracore_shared::auction::bid_increment(auction.highest_bid);
    let minimum = if is_buyout {
        None
    } else {
        let Ok(minimum) = minimum_next_bid(auction) else {
            return HoldDecision::Database;
        };
        Some(minimum)
    };
    if auction.highest_bid != 0 && request.offer <= auction.highest_bid {
        return HoldDecision::HigherBid {
            bidder_guid: auction.highest_bidder_guid,
            current_bid: auction.highest_bid,
            minimum_increment,
        };
    }
    if is_buyout {
        if seller_proceeds(auction.buyout, auction.deposit, auction.consignment_rate).is_none() {
            return HoldDecision::Database;
        }
        return HoldDecision::Accepted(BidAcceptance {
            price: auction.buyout,
            effect: AuctionBidEffect::SettleBuyout,
            displaced_bidder_guid: auction.highest_bidder_guid,
            displaced_bid: auction.highest_bid,
        });
    }
    let Some(minimum) = minimum else {
        return HoldDecision::Database;
    };
    if request.offer < minimum {
        return HoldDecision::BidIncrement;
    }
    if seller_proceeds(request.offer, auction.deposit, auction.consignment_rate).is_none() {
        return HoldDecision::Database;
    }
    let Some(revision) = auction.revision.checked_add(1) else {
        return HoldDecision::Database;
    };
    HoldDecision::Accepted(BidAcceptance {
        price: request.offer,
        effect: AuctionBidEffect::RemainActive { revision },
        displaced_bidder_guid: auction.highest_bidder_guid,
        displaced_bid: auction.highest_bid,
    })
}

/// Realm-core's Cancellation Gate (`cm:AuctionHouseHandler.cpp:403-468`). Only the seller cancels,
/// only an active listing of the auctioneer's market, and only at the cut the seller's Hold paid
/// for: a bid that landed after the Gateway read the listing changes the cut and refuses the
/// Cancellation, so the Hold refunds and the seller can try again.
fn decide_cancel(
    auction: Option<BidAuction>,
    request: HoldRequest,
    now_micros: i64,
) -> HoldDecision {
    if request.operation != HoldOperation::Cancel || request_is_malformed(&request) {
        return HoldDecision::Database;
    }
    let Some(auction) = active_listing(auction, &request, now_micros)
        .filter(|auction| auction.owner_guid == request.bidder_guid)
    else {
        return HoldDecision::ItemNotFound;
    };
    // A bid without a bidder, or a bidder without a bid, has no one to refund; expiry keeps the
    // same state for repair.
    if (auction.highest_bidder_guid == 0) != (auction.highest_bid == 0) {
        return HoldDecision::Database;
    }
    match auction_cut(auction.highest_bid, auction.consignment_rate) {
        Some(cut) if cut == request.offer => HoldDecision::Cancelled {
            cut,
            displaced_bidder_guid: auction.highest_bidder_guid,
            displaced_bid: auction.highest_bid,
        },
        _ => HoldDecision::Database,
    }
}

trait BidMarket {
    fn decision(&self, operation_id: u64) -> Option<(HoldRequest, HoldDecision)>;
    fn auction(&self, auction_id: u32) -> Option<BidAuction>;
    fn now_micros(&self) -> i64;
    fn commit_decision(
        &mut self,
        request: HoldRequest,
        auction: Option<BidAuction>,
        decision: HoldDecision,
    ) -> Result<(), AuctionRefusal>;
}

trait BidRefundSink {
    fn refund_decision(&self, operation_id: u64) -> Option<(HoldRequest, HoldDecision, u32)>;
    fn commit_refund(&mut self, request: HoldRequest, amount: u32) -> Result<(), AuctionRefusal>;
}

fn relay_bid_refund<S: BidRefundSink>(
    sink: &mut S,
    request: HoldRequest,
    amount: u32,
) -> Result<(), AuctionRefusal> {
    if amount == 0 {
        return Ok(());
    }
    let (existing_request, decision, recorded) = sink
        .refund_decision(request.operation_id)
        .ok_or(AuctionRefusal::Database)?;
    if existing_request != request
        || refundable_bid_value(decision, request.offer).is_none_or(|limit| amount > limit)
    {
        return Err(AuctionRefusal::Database);
    }
    if recorded != 0 {
        return if recorded == amount {
            Ok(())
        } else {
            Err(AuctionRefusal::Database)
        };
    }
    sink.commit_refund(request, amount)
}

fn confirm_bid_refund<S: BidSource>(
    source: &mut S,
    request: HoldRequest,
    amount: u32,
) -> Result<(), AuctionRefusal> {
    if amount == 0 {
        return Err(AuctionRefusal::Database);
    }
    let hold = source
        .hold(request.operation_id)
        .ok_or(AuctionRefusal::Database)?;
    let decision = hold.decision.ok_or(AuctionRefusal::Database)?;
    if hold.request != request
        || refundable_bid_value(decision, request.offer).is_none_or(|limit| amount > limit)
    {
        return Err(AuctionRefusal::Database);
    }
    if hold.deferred_refund == 0 {
        return Ok(());
    }
    if hold.deferred_refund != amount {
        return Err(AuctionRefusal::Database);
    }
    source.confirm_refund(request)
}

fn resolve_bid<S: BidMarket>(
    market: &mut S,
    request: HoldRequest,
) -> Result<HoldDecision, AuctionRefusal> {
    if let Some((existing_request, decision)) = market.decision(request.operation_id) {
        return if existing_request == request {
            Ok(decision)
        } else {
            Err(AuctionRefusal::Database)
        };
    }
    let auction = market.auction(request.auction_id);
    let decision = match request.operation {
        HoldOperation::Bid => decide_bid(auction, request, market.now_micros()),
        HoldOperation::Cancel => decide_cancel(auction, request, market.now_micros()),
    };
    market.commit_decision(request, auction, decision)?;
    Ok(decision)
}

fn drive_bid<S: BidSource, M: BidMarket>(
    source: &mut S,
    market: &mut M,
    request: HoldRequest,
) -> Result<HoldDecision, AuctionRefusal> {
    fence_bid(source, request)?;
    if let Some(decision) = source
        .hold(request.operation_id)
        .and_then(|hold| hold.decision)
    {
        return Ok(decision);
    }
    let decision = resolve_bid(market, request)?;
    finish_bid(source, request, decision)
}

trait ListingSource {
    fn seller_money(&self, seller_guid: u64) -> Option<u32>;
    fn item(&self, item_guid: u64) -> Option<ListingItem>;
    fn now_micros(&self) -> i64;
}

trait LocalListingSink: ListingSource {
    fn receipt(&self, operation_id: u64) -> Option<ListingReceipt>;
    fn commit_local(&mut self, listing: PreparedListing) -> u32;
}

fn operation_match<S: LocalListingSink>(sink: &S, request: ListingRequest) -> OperationMatch {
    match sink.receipt(request.operation_id) {
        None => OperationMatch::Fresh,
        Some(receipt) if receipt.auction_id != 0 && receipt.listing.request == request => {
            OperationMatch::Replay(receipt.auction_id)
        }
        Some(_) => OperationMatch::Conflict,
    }
}

fn prepare_from_source<S: ListingSource>(
    sink: &S,
    request: ListingRequest,
) -> Result<PreparedListing, AuctionRefusal> {
    if request.operation_id == 0 || request.house.id == 0 {
        return Err(AuctionRefusal::InvalidTerms);
    }
    let item = sink.item(request.item_guid);
    let seller_money = sink
        .seller_money(request.seller_guid)
        .ok_or(AuctionRefusal::ItemNotFound)?;
    let deposit = prepare_listing(
        item.as_ref(),
        request.seller_guid,
        seller_money,
        request.terms,
        request.house,
    )?;
    let created_micros = sink.now_micros();
    let expires_micros = i64::from(request.terms.duration_minutes)
        .checked_mul(MICROS_PER_MINUTE)
        .and_then(|duration| created_micros.checked_add(duration))
        .ok_or(AuctionRefusal::InvalidTerms)?;
    Ok(PreparedListing {
        request,
        snapshot: item.expect("validated listing item is present").snapshot,
        deposit,
        created_micros,
        expires_micros,
    })
}

fn create_local_listing<S: LocalListingSink>(
    sink: &mut S,
    request: ListingRequest,
) -> Result<u32, AuctionRefusal> {
    match operation_match(sink, request) {
        OperationMatch::Replay(auction_id) => return Ok(auction_id),
        OperationMatch::Conflict => return Err(AuctionRefusal::InvalidTerms),
        OperationMatch::Fresh => {}
    }
    let listing = prepare_from_source(sink, request)?;
    Ok(sink.commit_local(listing))
}

trait HoldSink: ListingSource {
    fn hold(&self, operation_id: u64) -> Option<ListingHold>;
    fn receipt(&self, operation_id: u64) -> Option<ListingReceipt>;
    fn commit_hold(&mut self, hold: ListingHold);
    fn confirm_hold(&mut self, receipt: ListingReceipt);
    fn delete_hold(&mut self, operation_id: u64);
}

trait ListingRefundSink {
    fn refund(&self, operation_id: u64) -> Result<Option<ListingRefund>, AuctionRefusal>;
    fn commit_refund(&mut self, refund: ListingRefund);
}

trait MarketSink {
    fn receipt(&self, operation_id: u64) -> Option<ListingReceipt>;
    fn commit_market(&mut self, listing: PreparedListing) -> u32;
}

fn fence_listing<S: HoldSink>(sink: &mut S, request: ListingRequest) -> Result<(), AuctionRefusal> {
    if let Some(receipt) = sink.receipt(request.operation_id) {
        return if receipt.auction_id != 0 && receipt.listing.request == request {
            Ok(())
        } else {
            Err(AuctionRefusal::InvalidTerms)
        };
    }
    if let Some(hold) = sink.hold(request.operation_id) {
        return if hold.listing.request == request {
            Ok(())
        } else {
            Err(AuctionRefusal::InvalidTerms)
        };
    }
    let listing = prepare_from_source(sink, request)?;
    sink.commit_hold(ListingHold { listing });
    Ok(())
}

fn commit_held_listing<S: MarketSink>(
    sink: &mut S,
    listing: PreparedListing,
) -> Result<u32, AuctionRefusal> {
    if let Some(receipt) = sink.receipt(listing.request.operation_id) {
        return if receipt.listing == listing {
            if receipt.auction_id == 0 {
                Err(AuctionRefusal::InvalidTerms)
            } else {
                Ok(receipt.auction_id)
            }
        } else {
            Err(AuctionRefusal::InvalidTerms)
        };
    }
    Ok(sink.commit_market(listing))
}

fn confirm_listing<S: HoldSink>(
    sink: &mut S,
    receipt: ListingReceipt,
) -> Result<(), AuctionRefusal> {
    if receipt.auction_id == 0 {
        return Err(AuctionRefusal::InvalidTerms);
    }
    if let Some(existing) = sink.receipt(receipt.listing.request.operation_id) {
        return if existing == receipt {
            Ok(())
        } else {
            Err(AuctionRefusal::InvalidTerms)
        };
    }
    let hold = sink
        .hold(receipt.listing.request.operation_id)
        .ok_or(AuctionRefusal::InvalidTerms)?;
    if hold.listing != receipt.listing {
        return Err(AuctionRefusal::InvalidTerms);
    }
    sink.confirm_hold(receipt);
    Ok(())
}

fn settle_listing<S: HoldSink>(sink: &mut S, operation_id: u64) -> Result<(), AuctionRefusal> {
    let receipt = sink
        .receipt(operation_id)
        .ok_or(AuctionRefusal::InvalidTerms)?;
    if receipt.auction_id == 0 {
        return Err(AuctionRefusal::InvalidTerms);
    }
    match sink.hold(operation_id) {
        None => Ok(()),
        Some(hold) if hold.listing == receipt.listing => {
            sink.delete_hold(operation_id);
            Ok(())
        }
        Some(_) => Err(AuctionRefusal::InvalidTerms),
    }
}

fn listing_refund(listing: &PreparedListing) -> ListingRefund {
    ListingRefund {
        listing: listing.clone(),
        mail: listing_release_mail(listing),
    }
}

fn refund_listing<S: ListingRefundSink>(
    sink: &mut S,
    refund: ListingRefund,
) -> Result<(), AuctionRefusal> {
    match sink.refund(refund.listing.request.operation_id)? {
        Some(existing) if existing == refund => Ok(()),
        Some(_) => Err(AuctionRefusal::InvalidTerms),
        None => {
            sink.commit_refund(refund);
            Ok(())
        }
    }
}

/// Give a Hold's value back after realm-core refused its listing. A Hold with a matching receipt
/// backs a live Auction and is never released; a missing Hold is a replay.
fn release_listing<S: HoldSink>(
    sink: &mut S,
    operation_id: u64,
    seller_guid: u64,
) -> Result<(), AuctionRefusal> {
    if sink.receipt(operation_id).is_some() {
        return Err(AuctionRefusal::InvalidTerms);
    }
    let Some(hold) = sink.hold(operation_id) else {
        return Ok(());
    };
    if hold.listing.request.seller_guid != seller_guid {
        return Err(AuctionRefusal::InvalidTerms);
    }
    sink.delete_hold(operation_id);
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ActiveAuction {
    id: u32,
    listing: PreparedListing,
    highest_bidder_guid: u64,
    highest_bid: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ExpiryCompletion {
    Unsold(AuctionMail),
    Sold([AuctionMail; 2]),
}

trait ExpirySink {
    fn auction(&self, auction_id: u32) -> Result<Option<ActiveAuction>, String>;
    fn complete_expiry(&mut self, auction: ActiveAuction, completion: ExpiryCompletion);
}

fn expiry_completion(auction: &ActiveAuction) -> Result<ExpiryCompletion, String> {
    match (auction.highest_bidder_guid, auction.highest_bid) {
        (0, 0) => Ok(ExpiryCompletion::Unsold(AuctionMail {
            recipient_guid: auction.listing.request.seller_guid,
            house: auction.listing.request.house.id,
            action: AuctionMailAction::Expired,
            item_entry: auction.listing.snapshot.entry,
            random_property_id: auction.listing.snapshot.random_property_id,
            money: 0,
            attached_item: auction.listing.snapshot,
            counterparty_guid: 0,
            bid: 0,
            buyout: 0,
            deposit: 0,
            cut: 0,
        })),
        (winner_guid, winning_price) if winner_guid != 0 && winning_price != 0 => {
            sale_settlement_mail(
                auction.listing.request.house.id,
                auction.listing.request.seller_guid,
                auction.listing.snapshot,
                winner_guid,
                winning_price,
                auction.listing.request.terms.buyout,
                auction.listing.deposit,
                auction.listing.request.house.consignment_rate,
            )
            .map(ExpiryCompletion::Sold)
            .ok_or_else(|| format!("auction {} sale settlement overflow", auction.id))
        }
        _ => Err(format!(
            "auction {} has inconsistent highest-bid state; preserving it for repair",
            auction.id
        )),
    }
}

fn expire_active<S: ExpirySink>(sink: &mut S, auction_id: u32) -> Result<(), String> {
    let Some(auction) = sink.auction(auction_id)? else {
        return Ok(());
    };
    let completion = expiry_completion(&auction)?;
    sink.complete_expiry(auction, completion);
    Ok(())
}

/// Reducer edge: only the tag crosses to the gateway; the detail stays in module logs.
fn refused(refusal: AuctionRefusal, detail: &str) -> String {
    let tag = refusal.as_tag();
    spacetimedb::log::info!("auction refused {tag}: {detail}");
    tag.to_string()
}

fn listing_from_hold(row: AuctionHold) -> PreparedListing {
    PreparedListing {
        request: ListingRequest {
            operation_id: row.operation_id,
            seller_guid: row.seller_guid,
            item_guid: row.item_guid,
            house: AuctionHousePolicy {
                id: row.house,
                deposit_rate: row.deposit_rate,
                consignment_rate: row.consignment_rate,
            },
            terms: ListingTerms {
                start_bid: row.start_bid,
                buyout: row.buyout,
                duration_minutes: row.duration_minutes,
            },
        },
        snapshot: crate::items::ItemSnapshot {
            entry: row.item_entry,
            stack_count: row.item_stack_count,
            durability: row.item_durability,
            enchant_id: row.item_enchant_id,
            soulbound: row.item_soulbound,
            random_property_id: row.random_property_id,
            item_text_id: row.item_text_id,
        },
        deposit: row.deposit,
        created_micros: row.created_micros,
        expires_micros: row.expires_micros,
    }
}

fn hold_from_listing(listing: PreparedListing) -> AuctionHold {
    AuctionHold {
        operation_id: listing.request.operation_id,
        seller_guid: listing.request.seller_guid,
        item_guid: listing.request.item_guid,
        item_entry: listing.snapshot.entry,
        item_stack_count: listing.snapshot.stack_count,
        item_durability: listing.snapshot.durability,
        item_enchant_id: listing.snapshot.enchant_id,
        item_soulbound: listing.snapshot.soulbound,
        random_property_id: listing.snapshot.random_property_id,
        item_text_id: listing.snapshot.item_text_id,
        start_bid: listing.request.terms.start_bid,
        buyout: listing.request.terms.buyout,
        duration_minutes: listing.request.terms.duration_minutes,
        deposit: listing.deposit,
        created_micros: listing.created_micros,
        expires_micros: listing.expires_micros,
        house: listing.request.house.id,
        deposit_rate: listing.request.house.deposit_rate,
        consignment_rate: listing.request.house.consignment_rate,
    }
}

fn listing_from_receipt(row: AuctionOperationReceipt) -> ListingReceipt {
    ListingReceipt {
        listing: PreparedListing {
            request: ListingRequest {
                operation_id: row.operation_id,
                seller_guid: row.actor_guid,
                item_guid: row.item_guid,
                house: AuctionHousePolicy {
                    id: row.house,
                    deposit_rate: row.deposit_rate,
                    consignment_rate: row.consignment_rate,
                },
                terms: ListingTerms {
                    start_bid: row.start_bid,
                    buyout: row.buyout,
                    duration_minutes: row.duration_minutes,
                },
            },
            snapshot: crate::items::ItemSnapshot {
                entry: row.item_entry,
                stack_count: row.item_stack_count,
                durability: row.item_durability,
                enchant_id: row.item_enchant_id,
                soulbound: row.item_soulbound,
                random_property_id: row.random_property_id,
                item_text_id: row.item_text_id,
            },
            deposit: row.deposit,
            created_micros: row.created_micros,
            expires_micros: row.expires_micros,
        },
        auction_id: row.auction_id,
    }
}

fn receipt_from_listing(listing: PreparedListing, auction_id: u32) -> AuctionOperationReceipt {
    AuctionOperationReceipt {
        operation_id: listing.request.operation_id,
        auction_id,
        actor_guid: listing.request.seller_guid,
        item_guid: listing.request.item_guid,
        item_entry: listing.snapshot.entry,
        item_stack_count: listing.snapshot.stack_count,
        item_durability: listing.snapshot.durability,
        item_enchant_id: listing.snapshot.enchant_id,
        item_soulbound: listing.snapshot.soulbound,
        random_property_id: listing.snapshot.random_property_id,
        item_text_id: listing.snapshot.item_text_id,
        start_bid: listing.request.terms.start_bid,
        buyout: listing.request.terms.buyout,
        duration_minutes: listing.request.terms.duration_minutes,
        deposit: listing.deposit,
        created_micros: listing.created_micros,
        expires_micros: listing.expires_micros,
        house: listing.request.house.id,
        deposit_rate: listing.request.house.deposit_rate,
        consignment_rate: listing.request.house.consignment_rate,
    }
}

struct CtxSource<'a> {
    ctx: &'a ReducerContext,
}

impl ListingSource for CtxSource<'_> {
    fn seller_money(&self, seller_guid: u64) -> Option<u32> {
        crate::helpers::acting_entity_by_guid(self.ctx, seller_guid).map(|seller| seller.money)
    }

    fn item(&self, item_guid: u64) -> Option<ListingItem> {
        let item = self.ctx.db.game_item_instance().guid().find(item_guid)?;
        let template = self.ctx.db.game_item_template().entry().find(item.entry)?;
        Some(ListingItem {
            guid: item.guid,
            owner_guid: item.owner_guid,
            slot: item.slot,
            mailable: crate::items::validate_bag_dest_slot(self.ctx, item.owner_guid, item.slot)
                .is_ok(),
            snapshot: crate::items::ItemSnapshot::from(&item),
            sell_price: template.sell_price,
        })
    }

    fn now_micros(&self) -> i64 {
        self.ctx.timestamp.to_micros_since_unix_epoch()
    }
}

fn insert_active_auction(ctx: &ReducerContext, listing: &PreparedListing) -> u32 {
    let created_at = Timestamp::from_micros_since_unix_epoch(listing.created_micros);
    let expires_at = Timestamp::from_micros_since_unix_epoch(listing.expires_micros);
    let auction = ctx.db.game_auction().insert(Auction {
        id: 0,
        listing_operation_id: listing.request.operation_id,
        house: listing.request.house.id,
        owner_guid: listing.request.seller_guid,
        item_guid: listing.request.item_guid,
        item_entry: listing.snapshot.entry,
        item_stack_count: listing.snapshot.stack_count,
        item_durability: listing.snapshot.durability,
        item_enchant_id: listing.snapshot.enchant_id,
        item_soulbound: listing.snapshot.soulbound,
        random_property_id: listing.snapshot.random_property_id,
        item_text_id: listing.snapshot.item_text_id,
        start_bid: listing.request.terms.start_bid,
        buyout: listing.request.terms.buyout,
        highest_bidder_guid: 0,
        highest_bid: 0,
        deposit: listing.deposit,
        created_at,
        expires_at,
        revision: 0,
        deposit_rate: listing.request.house.deposit_rate,
        consignment_rate: listing.request.house.consignment_rate,
    });
    ctx.db.game_auction_expiry().insert(AuctionExpiry {
        scheduled_id: 0,
        scheduled_at: ScheduleAt::Time(expires_at),
        auction_id: auction.id,
    });
    auction.id
}

fn consume_listing_value(ctx: &ReducerContext, listing: &PreparedListing) {
    let mut seller = crate::helpers::acting_entity_by_guid(ctx, listing.request.seller_guid)
        .expect("validated listing seller remains present in the reducer transaction");
    seller.money -= listing.deposit;
    ctx.db.game_world_entity().guid().update(seller);
    ctx.db
        .game_item_instance()
        .guid()
        .delete(listing.request.item_guid);
}

impl LocalListingSink for CtxSource<'_> {
    fn receipt(&self, operation_id: u64) -> Option<ListingReceipt> {
        self.ctx
            .db
            .game_auction_operation_receipt()
            .operation_id()
            .find(operation_id)
            .map(listing_from_receipt)
    }

    fn commit_local(&mut self, listing: PreparedListing) -> u32 {
        consume_listing_value(self.ctx, &listing);
        let auction_id = insert_active_auction(self.ctx, &listing);
        self.ctx
            .db
            .game_auction_operation_receipt()
            .insert(receipt_from_listing(listing, auction_id));
        auction_id
    }
}

impl HoldSink for CtxSource<'_> {
    fn hold(&self, operation_id: u64) -> Option<ListingHold> {
        self.ctx
            .db
            .game_auction_hold()
            .operation_id()
            .find(operation_id)
            .map(|row| ListingHold {
                listing: listing_from_hold(row),
            })
    }

    fn receipt(&self, operation_id: u64) -> Option<ListingReceipt> {
        <Self as LocalListingSink>::receipt(self, operation_id)
    }

    fn commit_hold(&mut self, hold: ListingHold) {
        consume_listing_value(self.ctx, &hold.listing);
        self.ctx
            .db
            .game_auction_hold()
            .insert(hold_from_listing(hold.listing));
    }

    fn confirm_hold(&mut self, receipt: ListingReceipt) {
        self.ctx
            .db
            .game_auction_operation_receipt()
            .insert(receipt_from_listing(receipt.listing, receipt.auction_id));
    }

    fn delete_hold(&mut self, operation_id: u64) {
        self.ctx
            .db
            .game_auction_hold()
            .operation_id()
            .delete(operation_id);
    }
}

struct CtxMarket<'a> {
    ctx: &'a ReducerContext,
}

struct CtxListingRefund<'a> {
    ctx: &'a ReducerContext,
}

impl ListingRefundSink for CtxListingRefund<'_> {
    fn refund(&self, operation_id: u64) -> Result<Option<ListingRefund>, AuctionRefusal> {
        let row = self
            .ctx
            .db
            .game_auction_operation_receipt()
            .operation_id()
            .find(operation_id);
        match row {
            None => Ok(None),
            Some(row) if row.auction_id == 0 => {
                Ok(Some(listing_refund(&listing_from_receipt(row).listing)))
            }
            Some(_) => Err(AuctionRefusal::InvalidTerms),
        }
    }

    fn commit_refund(&mut self, refund: ListingRefund) {
        insert_auction_mail(self.ctx, refund.mail);
        self.ctx
            .db
            .game_auction_operation_receipt()
            .insert(receipt_from_listing(refund.listing, 0));
    }
}

impl MarketSink for CtxMarket<'_> {
    fn receipt(&self, operation_id: u64) -> Option<ListingReceipt> {
        self.ctx
            .db
            .game_auction_operation_receipt()
            .operation_id()
            .find(operation_id)
            .map(listing_from_receipt)
    }

    fn commit_market(&mut self, listing: PreparedListing) -> u32 {
        let auction_id = insert_active_auction(self.ctx, &listing);
        self.ctx
            .db
            .game_auction_operation_receipt()
            .insert(receipt_from_listing(listing, auction_id));
        auction_id
    }
}

struct CtxBidSource<'a> {
    ctx: &'a ReducerContext,
}

impl BidSource for CtxBidSource<'_> {
    fn money(&self, bidder_guid: u64) -> Option<u32> {
        crate::helpers::acting_entity_by_guid(self.ctx, bidder_guid).map(|bidder| bidder.money)
    }

    fn hold(&self, operation_id: u64) -> Option<HeldBid> {
        let row = self
            .ctx
            .db
            .game_auction_bid_hold()
            .operation_id()
            .find(operation_id)?;
        Some(HeldBid {
            request: hold_request(
                HoldOperation::from_code(row.operation)?,
                row.operation_id,
                row.bidder_guid,
                row.auction_id,
                row.house,
                row.offer,
            ),
            decision: held_bid_decision(&row),
            deferred_refund: row.deferred_refund,
        })
    }

    fn create_hold(&mut self, request: HoldRequest) -> Result<(), AuctionRefusal> {
        let mut bidder = crate::helpers::acting_entity_by_guid(self.ctx, request.bidder_guid)
            .ok_or(AuctionRefusal::NotEnoughMoney)?;
        bidder.money = bidder
            .money
            .checked_sub(request.offer)
            .ok_or(AuctionRefusal::NotEnoughMoney)?;
        self.ctx.db.game_world_entity().guid().update(bidder);
        self.ctx.db.game_auction_bid_hold().insert(AuctionBidHold {
            operation_id: request.operation_id,
            bidder_guid: request.bidder_guid,
            auction_id: request.auction_id,
            house: request.house,
            offer: request.offer,
            outcome: BID_PENDING,
            revision: 0,
            result_bidder_guid: 0,
            result_bid: 0,
            minimum_increment: 0,
            accepted_price: 0,
            deferred_refund: 0,
            operation: request.operation.code(),
        });
        Ok(())
    }

    fn finish_hold(
        &mut self,
        request: HoldRequest,
        decision: HoldDecision,
    ) -> Result<(), AuctionRefusal> {
        let refund =
            refundable_bid_value(decision, request.offer).ok_or(AuctionRefusal::Database)?;
        let deferred_refund = if refund != 0 {
            let mut bidder = crate::helpers::acting_entity_by_guid(self.ctx, request.bidder_guid)
                .ok_or(AuctionRefusal::Database)?;
            let (money, deferred_refund) = crate::mail::split_refund(bidder.money, refund);
            bidder.money = money;
            self.ctx.db.game_world_entity().guid().update(bidder);
            deferred_refund
        } else {
            0
        };
        let fields = bid_decision_fields(decision);
        self.ctx
            .db
            .game_auction_bid_hold()
            .operation_id()
            .update(AuctionBidHold {
                operation_id: request.operation_id,
                bidder_guid: request.bidder_guid,
                auction_id: request.auction_id,
                house: request.house,
                offer: request.offer,
                outcome: fields.outcome,
                revision: fields.revision,
                result_bidder_guid: fields.result_bidder_guid,
                result_bid: fields.result_bid,
                minimum_increment: fields.minimum_increment,
                accepted_price: fields.accepted_price,
                deferred_refund,
                operation: request.operation.code(),
            });
        Ok(())
    }

    fn confirm_refund(&mut self, request: HoldRequest) -> Result<(), AuctionRefusal> {
        let mut row = self
            .ctx
            .db
            .game_auction_bid_hold()
            .operation_id()
            .find(request.operation_id)
            .ok_or(AuctionRefusal::Database)?;
        if row.operation != request.operation.code()
            || row.bidder_guid != request.bidder_guid
            || row.auction_id != request.auction_id
            || row.house != request.house
            || row.offer != request.offer
        {
            return Err(AuctionRefusal::Database);
        }
        row.deferred_refund = 0;
        self.ctx
            .db
            .game_auction_bid_hold()
            .operation_id()
            .update(row);
        Ok(())
    }
}

struct CtxBidMarket<'a> {
    ctx: &'a ReducerContext,
}

impl BidMarket for CtxBidMarket<'_> {
    fn decision(&self, operation_id: u64) -> Option<(HoldRequest, HoldDecision)> {
        let row = self
            .ctx
            .db
            .game_auction_bid_decision()
            .operation_id()
            .find(operation_id)?;
        Some((decided_request(&row)?, realm_bid_decision(&row)?))
    }

    fn auction(&self, auction_id: u32) -> Option<BidAuction> {
        self.ctx
            .db
            .game_auction()
            .id()
            .find(auction_id)
            .map(|auction| BidAuction {
                id: auction.id,
                house: auction.house,
                owner_guid: auction.owner_guid,
                item: crate::items::ItemSnapshot {
                    entry: auction.item_entry,
                    stack_count: auction.item_stack_count,
                    durability: auction.item_durability,
                    enchant_id: auction.item_enchant_id,
                    soulbound: auction.item_soulbound,
                    random_property_id: auction.random_property_id,
                    item_text_id: auction.item_text_id,
                },
                highest_bidder_guid: auction.highest_bidder_guid,
                highest_bid: auction.highest_bid,
                start_bid: auction.start_bid,
                buyout: auction.buyout,
                deposit: auction.deposit,
                consignment_rate: auction.consignment_rate,
                expires_micros: auction.expires_at.to_micros_since_unix_epoch(),
                revision: auction.revision,
            })
    }

    fn now_micros(&self) -> i64 {
        self.ctx.timestamp.to_micros_since_unix_epoch()
    }

    fn commit_decision(
        &mut self,
        request: HoldRequest,
        auction: Option<BidAuction>,
        decision: HoldDecision,
    ) -> Result<(), AuctionRefusal> {
        if let HoldDecision::Accepted(accepted) = decision {
            let expected = auction.ok_or(AuctionRefusal::Database)?;
            let mut row = self
                .ctx
                .db
                .game_auction()
                .id()
                .find(request.auction_id)
                .ok_or(AuctionRefusal::Database)?;
            if row.revision != expected.revision
                || row.highest_bidder_guid != expected.highest_bidder_guid
                || row.highest_bid != expected.highest_bid
            {
                return Err(AuctionRefusal::Database);
            }
            let refund_mail = displaced_bid_refund_mail(
                expected.house,
                expected.item,
                accepted.displaced_bidder_guid,
                accepted.displaced_bid,
            );
            outbid_notice(
                expected.house,
                request.auction_id,
                expected.item,
                accepted.displaced_bidder_guid,
                request.bidder_guid,
                accepted.displaced_bid,
            )
            .into_iter()
            .for_each(|notice| insert_auction_notice(self.ctx, notice));
            match accepted.effect {
                AuctionBidEffect::SettleBuyout => {
                    let sale_mail =
                        buyout_settlement_mail(expected, request.bidder_guid, accepted.price)
                            .ok_or(AuctionRefusal::Database)?;
                    self.ctx
                        .db
                        .game_auction_expiry()
                        .auction_id()
                        .delete(row.id);
                    self.ctx.db.game_auction().id().delete(row.id);
                    refund_mail
                        .into_iter()
                        .chain(sale_mail)
                        .for_each(|mail| insert_auction_mail(self.ctx, mail));
                    settlement_notices(
                        expected.house,
                        request.auction_id,
                        expected.item.entry,
                        expected.item.random_property_id,
                        expected.owner_guid,
                        request.bidder_guid,
                        accepted.price,
                    )
                    .into_iter()
                    .for_each(|notice| insert_auction_notice(self.ctx, notice));
                }
                AuctionBidEffect::RemainActive { revision } => {
                    row.highest_bidder_guid = request.bidder_guid;
                    row.highest_bid = accepted.price;
                    row.revision = revision;
                    self.ctx.db.game_auction().id().update(row);
                    refund_mail
                        .into_iter()
                        .for_each(|mail| insert_auction_mail(self.ctx, mail));
                    insert_auction_notice(
                        self.ctx,
                        new_bid_notice(
                            expected.house,
                            request.auction_id,
                            expected.item,
                            expected.owner_guid,
                            request.bidder_guid,
                            accepted.price,
                        ),
                    );
                }
            }
        }
        if let HoldDecision::Cancelled {
            displaced_bidder_guid,
            displaced_bid,
            ..
        } = decision
        {
            self.commit_cancellation(
                auction.ok_or(AuctionRefusal::Database)?,
                displaced_bidder_guid,
                displaced_bid,
            )?;
        }
        let fields = bid_decision_fields(decision);
        // The Auction row this decision is about may be deleted later (settled or expired) before
        // a purse-overflow refund on it fires. Keep the item the refund's subject needs here,
        // while the row is still guaranteed present.
        let (item_entry, random_property_id) =
            auction.map_or((0, 0), |a| (a.item.entry, a.item.random_property_id));
        self.ctx
            .db
            .game_auction_bid_decision()
            .insert(AuctionBidDecision {
                operation_id: request.operation_id,
                bidder_guid: request.bidder_guid,
                auction_id: request.auction_id,
                house: request.house,
                offer: request.offer,
                outcome: fields.outcome,
                revision: fields.revision,
                result_bidder_guid: fields.result_bidder_guid,
                result_bid: fields.result_bid,
                minimum_increment: fields.minimum_increment,
                accepted_price: fields.accepted_price,
                deferred_refund: 0,
                item_entry,
                random_property_id,
                operation: request.operation.code(),
            });
        Ok(())
    }
}

impl CtxBidMarket<'_> {
    /// Remove the listing the decision read, return its item to the seller, and refund the
    /// displaced bidder, all in the decision's transaction.
    fn commit_cancellation(
        &mut self,
        expected: BidAuction,
        displaced_bidder_guid: u64,
        displaced_bid: u32,
    ) -> Result<(), AuctionRefusal> {
        let row = self
            .ctx
            .db
            .game_auction()
            .id()
            .find(expected.id)
            .ok_or(AuctionRefusal::Database)?;
        if row.revision != expected.revision
            || row.highest_bidder_guid != displaced_bidder_guid
            || row.highest_bid != displaced_bid
        {
            return Err(AuctionRefusal::Database);
        }
        self.ctx
            .db
            .game_auction_expiry()
            .auction_id()
            .delete(row.id);
        self.ctx.db.game_auction().id().delete(row.id);
        cancellation_mail(expected, displaced_bidder_guid, displaced_bid)
            .into_iter()
            .for_each(|mail| insert_auction_mail(self.ctx, mail));
        removed_notice(
            expected.house,
            expected.id,
            expected.item,
            displaced_bidder_guid,
            displaced_bid,
        )
        .into_iter()
        .for_each(|notice| insert_auction_notice(self.ctx, notice));
        Ok(())
    }
}

impl BidRefundSink for CtxBidMarket<'_> {
    fn refund_decision(&self, operation_id: u64) -> Option<(HoldRequest, HoldDecision, u32)> {
        let row = self
            .ctx
            .db
            .game_auction_bid_decision()
            .operation_id()
            .find(operation_id)?;
        Some((
            decided_request(&row)?,
            realm_bid_decision(&row)?,
            row.deferred_refund,
        ))
    }

    fn commit_refund(&mut self, request: HoldRequest, amount: u32) -> Result<(), AuctionRefusal> {
        let mut row = self
            .ctx
            .db
            .game_auction_bid_decision()
            .operation_id()
            .find(request.operation_id)
            .ok_or(AuctionRefusal::Database)?;
        if decided_request(&row) != Some(request) || row.deferred_refund != 0 {
            return Err(AuctionRefusal::Database);
        }
        // The Auction this Hold was about may already be settled and gone (this refund is the
        // purse-overflow remainder of a bid that did not win, or of a refused Cancellation's cut).
        // No vanilla auction sends this mail at all; it renders as the closest client string,
        // Outbid, carrying the refund. The item comes from the decision row itself, not a fresh
        // Auction lookup, because the row this refund is about can outlive the Auction it was
        // decided against.
        insert_auction_mail(
            self.ctx,
            AuctionMail {
                recipient_guid: request.bidder_guid,
                house: request.house,
                action: AuctionMailAction::Outbid,
                item_entry: row.item_entry,
                random_property_id: row.random_property_id,
                money: amount,
                attached_item: crate::items::ItemSnapshot::default(),
                counterparty_guid: 0,
                bid: 0,
                buyout: 0,
                deposit: 0,
                cut: 0,
            },
        );
        row.deferred_refund = amount;
        self.ctx
            .db
            .game_auction_bid_decision()
            .operation_id()
            .update(row);
        Ok(())
    }
}

fn hold_request(
    operation: HoldOperation,
    operation_id: u64,
    bidder_guid: u64,
    auction_id: u32,
    house: u32,
    offer: u32,
) -> HoldRequest {
    HoldRequest {
        operation,
        operation_id,
        bidder_guid,
        auction_id,
        house,
        offer,
    }
}

/// The request a Realm-core decision row records. An operation code this Module does not know
/// reads as no request, so every comparison against it fails closed.
fn decided_request(row: &AuctionBidDecision) -> Option<HoldRequest> {
    Some(hold_request(
        HoldOperation::from_code(row.operation)?,
        row.operation_id,
        row.bidder_guid,
        row.auction_id,
        row.house,
        row.offer,
    ))
}

/// The operation a stored Hold or decision row pays for. The later phases take their operation
/// from the row, never from the caller, so one reducer finishes bids and Cancellations alike. A
/// missing row reads as a bid; the phase then finds no matching Hold and refuses.
fn stored_hold_operation(ctx: &ReducerContext, operation_id: u64) -> HoldOperation {
    ctx.db
        .game_auction_bid_hold()
        .operation_id()
        .find(operation_id)
        .map(|row| row.operation)
        .or_else(|| {
            ctx.db
                .game_auction_bid_decision()
                .operation_id()
                .find(operation_id)
                .map(|row| row.operation)
        })
        .and_then(HoldOperation::from_code)
        .unwrap_or(HoldOperation::Bid)
}

fn validate_market_listing(ctx: &ReducerContext, listing: &PreparedListing) -> Result<(), String> {
    if listing.request.operation_id == 0
        || listing.request.house.id == 0
        || !valid_rate(listing.request.house.deposit_rate)
        || !valid_rate(listing.request.house.consignment_rate)
        || listing.snapshot.stack_count == 0
        || listing.snapshot.soulbound
        || listing.request.terms.start_bid == 0
        || (listing.request.terms.buyout != 0
            && listing.request.terms.buyout < listing.request.terms.start_bid)
    {
        return Err(refused(
            AuctionRefusal::InvalidTerms,
            "invalid held listing",
        ));
    }
    let template = ctx
        .db
        .game_item_template()
        .entry()
        .find(listing.snapshot.entry)
        .ok_or_else(|| refused(AuctionRefusal::InvalidTerms, "item template missing"))?;
    let expected_deposit = listing_deposit(
        template.sell_price,
        listing.snapshot.stack_count,
        listing.request.terms.duration_minutes,
        listing.request.house.deposit_rate,
    )
    .ok_or_else(|| refused(AuctionRefusal::InvalidTerms, "invalid listing arithmetic"))?;
    let expected_expiry = i64::from(listing.request.terms.duration_minutes)
        .checked_mul(MICROS_PER_MINUTE)
        .and_then(|duration| listing.created_micros.checked_add(duration))
        .ok_or_else(|| refused(AuctionRefusal::InvalidTerms, "auction expiry overflow"))?;
    if listing.deposit != expected_deposit
        || !listing_proceeds_are_representable(
            listing.request.terms,
            expected_deposit,
            listing.request.house.consignment_rate,
        )
        || listing.expires_micros != expected_expiry
    {
        return Err(refused(
            AuctionRefusal::InvalidTerms,
            "held listing payload changed",
        ));
    }
    Ok(())
}

struct CtxExpiry<'a> {
    ctx: &'a ReducerContext,
}

impl ExpirySink for CtxExpiry<'_> {
    fn auction(&self, auction_id: u32) -> Result<Option<ActiveAuction>, String> {
        let Some(auction) = self.ctx.db.game_auction().id().find(auction_id) else {
            return Ok(None);
        };
        let receipt = self
            .ctx
            .db
            .game_auction_operation_receipt()
            .operation_id()
            .find(auction.listing_operation_id)
            .map(listing_from_receipt)
            .ok_or_else(|| {
                format!("auction {auction_id} has no operation receipt; preserving it for repair")
            })?;
        Ok(Some(ActiveAuction {
            id: auction.id,
            listing: receipt.listing,
            highest_bidder_guid: auction.highest_bidder_guid,
            highest_bid: auction.highest_bid,
        }))
    }

    fn complete_expiry(&mut self, auction: ActiveAuction, completion: ExpiryCompletion) {
        let house = auction.listing.request.house.id;
        let item = auction.listing.snapshot;
        let owner_guid = auction.listing.request.seller_guid;
        match completion {
            ExpiryCompletion::Unsold(mail) => {
                insert_auction_mail(self.ctx, mail);
                insert_auction_notice(
                    self.ctx,
                    expired_notice(house, auction.id, item, owner_guid),
                );
            }
            ExpiryCompletion::Sold(mail) => {
                mail.into_iter()
                    .for_each(|mail| insert_auction_mail(self.ctx, mail));
                settlement_notices(
                    house,
                    auction.id,
                    item.entry,
                    item.random_property_id,
                    owner_guid,
                    auction.highest_bidder_guid,
                    auction.highest_bid,
                )
                .into_iter()
                .for_each(|notice| insert_auction_notice(self.ctx, notice));
            }
        }
        self.ctx
            .db
            .game_auction_expiry()
            .auction_id()
            .delete(auction.id);
        self.ctx.db.game_auction().id().delete(auction.id);
    }
}

/// Single-database listing: item, deposit, Auction, receipt, and expiry are one transaction.
#[reducer]
#[allow(clippy::too_many_arguments)]
pub fn gw_auction_list_local(
    ctx: &ReducerContext,
    operation_id: u64,
    request_actor: crate::account_ownership::SessionActor,
    item_guid: u64,
    auctioneer_guid: u64,
    house: u32,
    start_bid: u32,
    buyout: u32,
    duration_minutes: u32,
) -> Result<(), String> {
    crate::helpers::require_operator(ctx)?;
    let seller_guid = crate::account_ownership::require_actor(ctx, request_actor)?;
    let requested_house = house;
    let existing_house = ctx
        .db
        .game_auction_operation_receipt()
        .operation_id()
        .find(operation_id)
        .map(listing_from_receipt)
        .map(|receipt| receipt.listing.request.house);
    let replay = existing_house.is_some();
    let house = match existing_house {
        Some(policy) if policy.id == requested_house => policy,
        Some(_) => {
            return Err(refused(
                AuctionRefusal::InvalidTerms,
                "listing operation id conflict",
            ));
        }
        None => imported_house_policy(ctx, requested_house)?,
    };
    if !replay && auction_house_for_interaction(ctx, seller_guid, auctioneer_guid) != Some(house) {
        return Err(refused(
            AuctionRefusal::InvalidTerms,
            "auctioneer refused interaction",
        ));
    }
    create_local_listing(
        &mut CtxSource { ctx },
        ListingRequest {
            operation_id,
            seller_guid,
            item_guid,
            house,
            terms: ListingTerms {
                start_bid,
                buyout,
                duration_minutes,
            },
        },
    )
    .map(|_| ())
    .map_err(|refusal| refused(refusal, "listing rejected"))
}

/// Sharded listing: atomically move the source value into a caller-identified Hold.
#[reducer]
#[allow(clippy::too_many_arguments)]
pub fn gw_auction_hold_listing(
    ctx: &ReducerContext,
    operation_id: u64,
    request_actor: crate::account_ownership::SessionActor,
    item_guid: u64,
    auctioneer_guid: u64,
    house: u32,
    start_bid: u32,
    buyout: u32,
    duration_minutes: u32,
) -> Result<(), String> {
    crate::helpers::require_operator(ctx)?;
    let seller_guid = crate::account_ownership::require_actor(ctx, request_actor)?;
    let requested_house = house;
    let existing_house = ctx
        .db
        .game_auction_hold()
        .operation_id()
        .find(operation_id)
        .map(listing_from_hold)
        .map(|listing| listing.request.house)
        .or_else(|| {
            ctx.db
                .game_auction_operation_receipt()
                .operation_id()
                .find(operation_id)
                .map(listing_from_receipt)
                .map(|receipt| receipt.listing.request.house)
        });
    let replay = existing_house.is_some();
    let house = match existing_house {
        Some(policy) if policy.id == requested_house => policy,
        Some(_) => {
            return Err(refused(
                AuctionRefusal::InvalidTerms,
                "listing operation id conflict",
            ));
        }
        None => imported_house_policy(ctx, requested_house)?,
    };
    if !replay && auction_house_for_interaction(ctx, seller_guid, auctioneer_guid) != Some(house) {
        return Err(refused(
            AuctionRefusal::InvalidTerms,
            "auctioneer refused interaction",
        ));
    }
    fence_listing(
        &mut CtxSource { ctx },
        ListingRequest {
            operation_id,
            seller_guid,
            item_guid,
            house,
            terms: ListingTerms {
                start_bid,
                buyout,
                duration_minutes,
            },
        },
    )
    .map_err(|refusal| refused(refusal, "listing Hold rejected"))
}

/// Sharded listing: create the realm Auction and idempotency receipt from a held payload.
#[reducer]
#[allow(clippy::too_many_arguments)]
pub fn realm_auction_commit_listing(
    ctx: &ReducerContext,
    operation_id: u64,
    request_actor: crate::account_ownership::SessionActor,
    item_guid: u64,
    item_entry: u32,
    item_stack_count: u32,
    item_durability: u32,
    item_enchant_id: u32,
    item_soulbound: bool,
    random_property_id: u32,
    house: u32,
    deposit_rate: u32,
    consignment_rate: u32,
    start_bid: u32,
    buyout: u32,
    duration_minutes: u32,
    deposit: u32,
    created_micros: i64,
    expires_micros: i64,
    item_text_id: u32,
) -> Result<(), String> {
    crate::helpers::require_operator(ctx)?;
    let seller_guid = crate::account_ownership::require_actor(ctx, request_actor)?;
    let listing = PreparedListing {
        request: ListingRequest {
            operation_id,
            seller_guid,
            item_guid,
            house: AuctionHousePolicy {
                id: house,
                deposit_rate,
                consignment_rate,
            },
            terms: ListingTerms {
                start_bid,
                buyout,
                duration_minutes,
            },
        },
        snapshot: crate::items::ItemSnapshot {
            entry: item_entry,
            stack_count: item_stack_count,
            durability: item_durability,
            enchant_id: item_enchant_id,
            soulbound: item_soulbound,
            random_property_id,
            item_text_id,
        },
        deposit,
        created_micros,
        expires_micros,
    };
    let mut market = CtxMarket { ctx };
    if market.receipt(operation_id).is_none() {
        validate_market_listing(ctx, &listing)?;
    }
    commit_held_listing(&mut market, listing)
        .map(|_| ())
        .map_err(|refusal| refused(refusal, "listing operation id conflict"))
}

fn require_listing_actor(
    ctx: &ReducerContext,
    operation_id: u64,
    actor: crate::account_ownership::SessionActor,
) -> Result<(), String> {
    crate::account_ownership::require_actor(ctx, actor)?;
    let seller = CtxSource { ctx }
        .hold(operation_id)
        .map(|hold| hold.listing.request.seller_guid)
        .or_else(|| {
            <CtxSource<'_> as HoldSink>::receipt(&CtxSource { ctx }, operation_id)
                .map(|receipt| receipt.listing.request.seller_guid)
        });
    if let Some(guid) = seller {
        crate::account_ownership::require_actor_for(ctx, actor, guid)?;
    }
    Ok(())
}

/// Sharded listing: copy the matching realm receipt onto the source shard.
#[reducer]
pub fn realm_auction_confirm_listing(
    ctx: &ReducerContext,
    operation_id: u64,
    auction_id: u32,
    request_actor: crate::account_ownership::SessionActor,
) -> Result<(), String> {
    crate::helpers::require_operator(ctx)?;
    require_listing_actor(ctx, operation_id, request_actor)?;
    let listing = CtxSource { ctx }
        .hold(operation_id)
        .map(|hold| hold.listing)
        .or_else(|| {
            <CtxSource<'_> as HoldSink>::receipt(&CtxSource { ctx }, operation_id)
                .map(|receipt| receipt.listing)
        })
        .ok_or_else(|| refused(AuctionRefusal::InvalidTerms, "listing Hold missing"))?;
    confirm_listing(
        &mut CtxSource { ctx },
        ListingReceipt {
            listing,
            auction_id,
        },
    )
    .map_err(|refusal| refused(refusal, "listing receipt conflict"))
}

/// Sharded listing: delete the Hold only after the source has matching receipt evidence.
#[reducer]
pub fn realm_auction_settle_listing(
    ctx: &ReducerContext,
    operation_id: u64,
    request_actor: crate::account_ownership::SessionActor,
) -> Result<(), String> {
    crate::helpers::require_operator(ctx)?;
    require_listing_actor(ctx, operation_id, request_actor)?;
    settle_listing(&mut CtxSource { ctx }, operation_id)
        .map_err(|refusal| refused(refusal, "listing Hold is not confirmed"))
}

/// Sharded listing abort: Realm-core commits the seller's exact return Mail and durable
/// refund receipt together. Replays with the same payload do not create a second Mail.
#[reducer]
#[allow(clippy::too_many_arguments)] // The persisted refund receipt's value columns.
pub fn realm_auction_refund_listing(
    ctx: &ReducerContext,
    operation_id: u64,
    request_actor: crate::account_ownership::SessionActor,
    item_guid: u64,
    item_entry: u32,
    item_stack_count: u32,
    item_durability: u32,
    item_enchant_id: u32,
    item_soulbound: bool,
    random_property_id: u32,
    house: u32,
    deposit_rate: u32,
    consignment_rate: u32,
    start_bid: u32,
    buyout: u32,
    duration_minutes: u32,
    deposit: u32,
    created_micros: i64,
    expires_micros: i64,
    item_text_id: u32,
) -> Result<(), String> {
    crate::helpers::require_operator(ctx)?;
    let seller_guid = crate::account_ownership::require_actor(ctx, request_actor)?;
    refund_listing(
        &mut CtxListingRefund { ctx },
        listing_refund(&listing_from_hold(AuctionHold {
            operation_id,
            seller_guid,
            item_guid,
            item_entry,
            item_stack_count,
            item_durability,
            item_enchant_id,
            item_soulbound,
            random_property_id,
            house,
            deposit_rate,
            consignment_rate,
            start_bid,
            buyout,
            duration_minutes,
            deposit,
            created_micros,
            expires_micros,
            item_text_id,
        })),
    )
    .map_err(|refusal| refused(refusal, "listing refund conflict"))
}

/// Sharded listing abort: after realm-core refuses, mail the held item and deposit back
/// to the seller and delete the Hold. The gateway calls this only after Realm-core has committed
/// the matching refund receipt and Mail. Refused once the Hold has a receipt.
#[reducer]
pub fn gw_auction_release_listing_hold(
    ctx: &ReducerContext,
    operation_id: u64,
    request_actor: crate::account_ownership::SessionActor,
) -> Result<(), String> {
    crate::helpers::require_operator(ctx)?;
    let seller_guid = crate::account_ownership::require_actor(ctx, request_actor)?;
    release_listing(&mut CtxSource { ctx }, operation_id, seller_guid)
        .map_err(|refusal| refused(refusal, "listing Hold is confirmed"))
}

/// The Home Shard half every Hold starts with: the interaction Gate on a fresh operation, then the
/// fence. A replay skips the Gate, because the Hold already proves it passed.
fn gate_and_fence(
    ctx: &ReducerContext,
    request: HoldRequest,
    auctioneer_guid: u64,
) -> Result<(), String> {
    let replay = ctx
        .db
        .game_auction_bid_hold()
        .operation_id()
        .find(request.operation_id)
        .is_some();
    if !replay
        && auction_house_for_interaction(ctx, request.bidder_guid, auctioneer_guid)
            .is_none_or(|policy| policy.id != request.house)
    {
        return Err(refused(
            AuctionRefusal::Database,
            "auctioneer refused interaction",
        ));
    }
    fence_bid(&mut CtxBidSource { ctx }, request)
        .map_err(|refusal| refused(refusal, "Hold rejected"))
}

/// Single-database Hold: the fence, the realm decision, its Auction and mail effects, the terminal
/// source outcome and any purse-overflow refund mail commit in one transaction.
fn drive_local_hold(
    ctx: &ReducerContext,
    request: HoldRequest,
    auctioneer_guid: u64,
) -> Result<(), String> {
    gate_and_fence(ctx, request, auctioneer_guid)?;
    drive_bid(
        &mut CtxBidSource { ctx },
        &mut CtxBidMarket { ctx },
        request,
    )
    .map_err(|refusal| refused(refusal, "local Hold rejected"))?;
    let deferred_refund = CtxBidSource { ctx }
        .hold(request.operation_id)
        .ok_or_else(|| refused(AuctionRefusal::Database, "local Hold missing"))?
        .deferred_refund;
    if deferred_refund != 0 {
        relay_bid_refund(&mut CtxBidMarket { ctx }, request, deferred_refund)
            .map_err(|refusal| refused(refusal, "local Hold refund conflict"))?;
        confirm_bid_refund(&mut CtxBidSource { ctx }, request, deferred_refund)
            .map_err(|refusal| refused(refusal, "local Hold refund confirmation conflict"))?;
    }
    Ok(())
}

/// Single-database bid: full-offer Hold, realm decision, Auction update or buyout settlement,
/// ordinary mail, and terminal source outcome commit atomically.
#[reducer]
pub fn gw_auction_bid_local(
    ctx: &ReducerContext,
    operation_id: u64,
    request_actor: crate::account_ownership::SessionActor,
    auctioneer_guid: u64,
    auction_id: u32,
    house: u32,
    offer: u32,
) -> Result<(), String> {
    crate::helpers::require_operator(ctx)?;
    let bidder_guid = crate::account_ownership::require_actor(ctx, request_actor)?;
    let request = hold_request(
        HoldOperation::Bid,
        operation_id,
        bidder_guid,
        auction_id,
        house,
        offer,
    );
    drive_local_hold(ctx, request, auctioneer_guid)
}

/// Single-database Cancellation: the cut's Hold, the realm decision, the listing's removal, its
/// Cancelled and refund mail, and the terminal source outcome commit atomically.
#[reducer]
pub fn gw_auction_cancel_local(
    ctx: &ReducerContext,
    operation_id: u64,
    request_actor: crate::account_ownership::SessionActor,
    auctioneer_guid: u64,
    auction_id: u32,
    house: u32,
    cut: u32,
) -> Result<(), String> {
    crate::helpers::require_operator(ctx)?;
    let seller_guid = crate::account_ownership::require_actor(ctx, request_actor)?;
    let request = hold_request(
        HoldOperation::Cancel,
        operation_id,
        seller_guid,
        auction_id,
        house,
        cut,
    );
    drive_local_hold(ctx, request, auctioneer_guid)
}

/// Sharded bid: move the complete offer into a source-shard Hold before realm-core decides.
#[reducer]
pub fn gw_auction_hold_bid(
    ctx: &ReducerContext,
    operation_id: u64,
    request_actor: crate::account_ownership::SessionActor,
    auctioneer_guid: u64,
    auction_id: u32,
    house: u32,
    offer: u32,
) -> Result<(), String> {
    crate::helpers::require_operator(ctx)?;
    let bidder_guid = crate::account_ownership::require_actor(ctx, request_actor)?;
    let request = hold_request(
        HoldOperation::Bid,
        operation_id,
        bidder_guid,
        auction_id,
        house,
        offer,
    );
    gate_and_fence(ctx, request, auctioneer_guid)
}

/// Sharded Cancellation: move the Auction Cut into a source-shard Hold before realm-core
/// decides. A seller who cannot pay it is refused with nothing held.
#[reducer]
pub fn gw_auction_hold_cancel(
    ctx: &ReducerContext,
    operation_id: u64,
    request_actor: crate::account_ownership::SessionActor,
    auctioneer_guid: u64,
    auction_id: u32,
    house: u32,
    cut: u32,
) -> Result<(), String> {
    crate::helpers::require_operator(ctx)?;
    let seller_guid = crate::account_ownership::require_actor(ctx, request_actor)?;
    let request = hold_request(
        HoldOperation::Cancel,
        operation_id,
        seller_guid,
        auction_id,
        house,
        cut,
    );
    gate_and_fence(ctx, request, auctioneer_guid)
}

/// Sharded bid: serialize against the realm Auction and persist one terminal decision.
#[reducer]
pub fn realm_auction_decide_bid(
    ctx: &ReducerContext,
    operation_id: u64,
    request_actor: crate::account_ownership::SessionActor,
    auction_id: u32,
    house: u32,
    offer: u32,
) -> Result<(), String> {
    crate::helpers::require_operator(ctx)?;
    let bidder_guid = crate::account_ownership::require_actor(ctx, request_actor)?;
    resolve_bid(
        &mut CtxBidMarket { ctx },
        hold_request(
            HoldOperation::Bid,
            operation_id,
            bidder_guid,
            auction_id,
            house,
            offer,
        ),
    )
    .map(|_| ())
    .map_err(|refusal| refused(refusal, "bid decision conflict"))
}

/// Sharded Cancellation: serialize against the realm Auction and persist one terminal
/// decision. A Cancelled decision removes the listing and writes its mail and notice in the same
/// transaction; a replay changes nothing.
#[reducer]
pub fn realm_auction_decide_cancel(
    ctx: &ReducerContext,
    operation_id: u64,
    request_actor: crate::account_ownership::SessionActor,
    auction_id: u32,
    house: u32,
    cut: u32,
) -> Result<(), String> {
    crate::helpers::require_operator(ctx)?;
    let seller_guid = crate::account_ownership::require_actor(ctx, request_actor)?;
    resolve_bid(
        &mut CtxBidMarket { ctx },
        hold_request(
            HoldOperation::Cancel,
            operation_id,
            seller_guid,
            auction_id,
            house,
            cut,
        ),
    )
    .map(|_| ())
    .map_err(|refusal| refused(refusal, "Cancellation decision conflict"))
}

/// Sharded for a bid or a Cancellation: consume the accepted price or the cut, or restore
/// refused value, exactly once. The operation comes from the stored Hold.
#[reducer]
#[allow(clippy::too_many_arguments)]
pub fn gw_auction_finish_bid(
    ctx: &ReducerContext,
    operation_id: u64,
    request_actor: crate::account_ownership::SessionActor,
    auction_id: u32,
    house: u32,
    offer: u32,
    outcome: u8,
    revision: u64,
    result_bidder_guid: u64,
    result_bid: u32,
    minimum_increment: u32,
    accepted_price: u32,
) -> Result<(), String> {
    crate::helpers::require_operator(ctx)?;
    let bidder_guid = crate::account_ownership::require_actor(ctx, request_actor)?;
    let decision = bid_decision_from_fields(
        BidDecisionFields {
            outcome,
            revision,
            result_bidder_guid,
            result_bid,
            minimum_increment,
            accepted_price,
        },
        offer,
    )
    .ok_or_else(|| refused(AuctionRefusal::Database, "Hold decision is pending"))?;
    finish_bid(
        &mut CtxBidSource { ctx },
        hold_request(
            stored_hold_operation(ctx, operation_id),
            operation_id,
            bidder_guid,
            auction_id,
            house,
            offer,
        ),
        decision,
    )
    .map(|_| ())
    .map_err(|refusal| refused(refusal, "Hold outcome conflict"))
}

/// Sharded for a bid or a Cancellation: place an unrepresentable purse refund in
/// realm-core mail exactly once. The operation comes from the stored decision.
#[reducer]
pub fn realm_auction_refund_bid(
    ctx: &ReducerContext,
    operation_id: u64,
    request_actor: crate::account_ownership::SessionActor,
    auction_id: u32,
    house: u32,
    offer: u32,
    deferred_refund: u32,
) -> Result<(), String> {
    crate::helpers::require_operator(ctx)?;
    let bidder_guid = crate::account_ownership::require_actor(ctx, request_actor)?;
    relay_bid_refund(
        &mut CtxBidMarket { ctx },
        hold_request(
            stored_hold_operation(ctx, operation_id),
            operation_id,
            bidder_guid,
            auction_id,
            house,
            offer,
        ),
        deferred_refund,
    )
    .map_err(|refusal| refused(refusal, "Hold refund conflict"))
}

/// Sharded for a bid or a Cancellation: record on the source that realm-core durably
/// accepted the refund mail. The operation comes from the stored Hold.
#[reducer]
pub fn gw_auction_confirm_bid_refund(
    ctx: &ReducerContext,
    operation_id: u64,
    request_actor: crate::account_ownership::SessionActor,
    auction_id: u32,
    house: u32,
    offer: u32,
    deferred_refund: u32,
) -> Result<(), String> {
    crate::helpers::require_operator(ctx)?;
    let bidder_guid = crate::account_ownership::require_actor(ctx, request_actor)?;
    confirm_bid_refund(
        &mut CtxBidSource { ctx },
        hold_request(
            stored_hold_operation(ctx, operation_id),
            operation_id,
            bidder_guid,
            auction_id,
            house,
            offer,
        ),
        deferred_refund,
    )
    .map_err(|refusal| refused(refusal, "Hold refund confirmation conflict"))
}

/// Scheduler-only one-shot expiry. Replays see no active Auction and therefore create no mail.
#[reducer]
pub fn expire_auction(ctx: &ReducerContext, schedule: AuctionExpiry) -> Result<(), String> {
    if ctx.sender() != ctx.database_identity() {
        return Err("scheduler only".to_string());
    }
    expire_active(&mut CtxExpiry { ctx }, schedule.auction_id)
}

/// Character deletion must not destroy value held by or listed for that character.
pub(crate) fn character_has_auction_value(ctx: &ReducerContext, character_guid: u64) -> bool {
    ctx.db
        .game_auction_bid_hold()
        .by_bidder()
        .filter(character_guid)
        .any(|hold| hold.outcome == BID_PENDING || hold.deferred_refund != 0)
        || ctx
            .db
            .game_auction_hold()
            .by_seller()
            .filter(character_guid)
            .next()
            .is_some()
        || ctx
            .db
            .game_auction()
            .by_owner()
            .filter(character_guid)
            .next()
            .is_some()
        || ctx
            .db
            .game_auction()
            .by_highest_bidder()
            .filter(character_guid)
            .next()
            .is_some()
}

/// The exact English subjects LyraCore's earlier auction code sent, each a `Character` mail with
/// no vanilla twin. A real player's letter can carry the same words, so the subject alone never
/// authorizes a re-tag; [`legacy_mail_matches_shape`] is the rest of the check.
const LEGACY_AUCTION_SUBJECTS: &[(&str, AuctionMailAction)] = &[
    ("Auction outbid", AuctionMailAction::Outbid),
    ("Auction won", AuctionMailAction::Won),
    ("Auction sold", AuctionMailAction::Successful),
    ("Auction expired", AuctionMailAction::Expired),
    ("Auction listing refused", AuctionMailAction::Cancelled),
    // The deferred bid refund shares no subject with the ordinary Outbid mail, so it needs its
    // own row here; it also renders as Outbid (see `CtxBidMarket::commit_refund`).
    ("Auction bid refund", AuctionMailAction::Outbid),
];

/// The pure half of the repair: which `AuctionMailAction` a legacy row's exact English subject
/// maps to, or `None` for a subject this repair does not recognize.
fn legacy_auction_action(subject: &str) -> Option<AuctionMailAction> {
    LEGACY_AUCTION_SUBJECTS
        .iter()
        .find(|(known, _)| *known == subject)
        .map(|(_, action)| *action)
}

/// Whether `mail` carries the exact field shape the earlier auction code produced for its
/// subject — the first of two checks that tell a real letter apart from a look-alike. A player's
/// own mail can share the subject text (nothing stops them from titling a letter "Auction won"),
/// but every legacy row also carries `cod == 0` and an empty body (auction mail never used
/// either), and:
/// - Outbid, Expired, Cancelled and the deferred bid refund came from guid 0 — no player-sent
///   letter ever does, since a send always stamps the sender's own actor guid. That alone rules
///   out a look-alike for these four subjects.
/// - Expired and Cancelled carry an item; Outbid and the bid refund carry none.
/// - Won carries an item and no money; Sold carries money and no item. Neither has a guid-0 tell —
///   a real player letter can be titled either with a real sender and the right shape — so
///   [`legacy_repair_authorization`] is what actually clears them, not this function.
fn legacy_mail_matches_shape(mail: &crate::Mail) -> bool {
    if mail.cod != 0 || !mail.body.is_empty() {
        return false;
    }
    let has_item = !mail.snapshot().is_empty();
    match mail.subject.as_str() {
        "Auction outbid" | "Auction bid refund" => mail.sender_guid == 0 && !has_item,
        "Auction expired" => mail.sender_guid == 0 && has_item && mail.money == 0,
        "Auction listing refused" => mail.sender_guid == 0 && has_item && mail.money != 0,
        "Auction won" => has_item && mail.money == 0,
        "Auction sold" => !has_item && mail.money != 0,
        _ => false,
    }
}

/// A converted row's house and the item reference its vanilla subject encodes.
struct LegacyRepairTarget {
    house: u32,
    item_entry: u32,
    random_property_id: u32,
}

/// The second, and for Won/Sold the decisive, check: whether a durable listing receipt backs
/// `mail`'s claim, and if so, the house and subject-item that receipt supplies. Every other legacy
/// subject already cleared [`legacy_mail_matches_shape`]'s guid-0 tell, so a receipt only sharpens
/// its house; Won and Sold do not have that tell, because nothing else in the mail row tells a
/// real settlement apart from a same-titled player letter with an unrelated real sender:
/// - Won: some receipt's actor is the mail's claimed sender (the seller who listed this exact
///   item) and its item entry and stack count match what the mail carries. A look-alike sent by an
///   uninvolved player never has a receipt naming them as that item's seller.
/// - Sold: some receipt's actor is the mail's recipient and its `auction_id` is not the
///   refused-listing sentinel (0) — proceeds mail carries no item of its own to cross-check, so a
///   refused listing's refund receipt is not a settlement at all. A seller can have more than one
///   real receipt, so when more than one candidate remains, only the one whose listing terms could
///   have produced this exact proceeds figure ([`receipt_could_pay_out`]) is trusted; if that still
///   leaves more than one, or none, the row is left unmapped rather than guessed at. The subject
///   also borrows the winning receipt's item, since the mail's own item fields are 0 (Sold never
///   attaches the item).
/// - Expired and Cancelled: some receipt's actor is the mail's recipient (the seller the item
///   returned to) and its item entry and stack count match what the mail carries, the same
///   assurance Won uses. A `Cancelled` (refused-listing) row's own receipt IS the `auction_id == 0`
///   sentinel, so unlike Sold this does not exclude it.
///
/// Every subject above takes its house from that receipt rather than the recipient's own race
/// (`house_for_faction_template`'s approach for a live sale,
/// `crates/lyracore-shared/src/auction.rs`): Realm-core, the database this repair runs against,
/// carries no Character rows at all, so a race lookup would always miss and fall back to the
/// neutral house. A receipt's `house` is durable state recorded when the listing was created, so
/// it needs no Character lookup and is correct on Realm-core too. Expired and Cancelled fall back
/// to the race lookup only if no receipt matches their item (a legacy row that predates receipts,
/// say); Outbid and the deferred bid refund have no receipt to draw from at all — a bidder is never
/// a receipt's actor — and always use the race lookup.
fn legacy_repair_authorization(
    ctx: &ReducerContext,
    mail: &crate::Mail,
    action: AuctionMailAction,
) -> Option<LegacyRepairTarget> {
    match action {
        AuctionMailAction::Won => {
            let receipt = ctx
                .db
                .game_auction_operation_receipt()
                .by_actor()
                .filter(&mail.sender_guid)
                .find(|receipt| {
                    receipt.item_entry == mail.item_entry
                        && receipt.item_stack_count == mail.item_stack_count
                })?;
            Some(LegacyRepairTarget {
                house: receipt.house,
                item_entry: mail.item_entry,
                random_property_id: mail.random_property_id,
            })
        }
        AuctionMailAction::Successful => {
            let candidates: Vec<AuctionOperationReceipt> = ctx
                .db
                .game_auction_operation_receipt()
                .by_actor()
                .filter(&mail.recipient_guid)
                .filter(|receipt| receipt.auction_id != 0)
                .collect();
            let receipt = match candidates.len() {
                0 => return None,
                1 => candidates.into_iter().next()?,
                _ => {
                    let mut paying = candidates
                        .into_iter()
                        .filter(|receipt| receipt_could_pay_out(receipt, mail.money));
                    let only = paying.next()?;
                    if paying.next().is_some() {
                        return None; // still ambiguous — more than one listing could have paid this
                    }
                    only
                }
            };
            Some(LegacyRepairTarget {
                house: receipt.house,
                item_entry: receipt.item_entry,
                random_property_id: receipt.random_property_id,
            })
        }
        AuctionMailAction::Expired | AuctionMailAction::Cancelled => {
            let house = ctx
                .db
                .game_auction_operation_receipt()
                .by_actor()
                .filter(&mail.recipient_guid)
                .find(|receipt| {
                    receipt.item_entry == mail.item_entry
                        && receipt.item_stack_count == mail.item_stack_count
                })
                .map_or_else(
                    || legacy_character_house(ctx, mail.recipient_guid),
                    |r| r.house,
                );
            Some(LegacyRepairTarget {
                house,
                item_entry: mail.item_entry,
                random_property_id: mail.random_property_id,
            })
        }
        _ => Some(LegacyRepairTarget {
            house: legacy_character_house(ctx, mail.recipient_guid),
            item_entry: mail.item_entry,
            random_property_id: mail.random_property_id,
        }),
    }
}

/// Whether some winning price within `receipt`'s listed range (`start_bid..=buyout`, or
/// `start_bid..` when `buyout` is 0 — an auction the vanilla protocol lets bidding pass without a
/// cap) pays the seller exactly `money`. `seller_proceeds` is non-decreasing in price and never
/// skips a whole copper as price climbs by one (its cut grows by at most one copper per copper of
/// price), so every integer between its low and high ends is reachable — checking the two ends
/// bounds every price in between too.
fn receipt_could_pay_out(receipt: &AuctionOperationReceipt, money: u32) -> bool {
    let Some(low) = seller_proceeds(receipt.start_bid, receipt.deposit, receipt.consignment_rate)
    else {
        return false;
    };
    if money < low {
        return false;
    }
    if receipt.buyout == 0 {
        return true;
    }
    match seller_proceeds(receipt.buyout, receipt.deposit, receipt.consignment_rate) {
        Some(high) => money <= high,
        None => true, // the buyout's own proceeds overflow u32; some in-range price still might not
    }
}

/// The house a converted row shows the client on a deployment where Realm-core's Character rows
/// exist, chosen from the recipient's own team: Alliance goes to Stormwind (1), Horde to Orgrimmar
/// (6), matching `house_for_faction_template`'s own team fallback
/// (`crates/lyracore-shared/src/auction.rs`). Falls back to the neutral house (7) for a recipient
/// with no Character row — the same fallback that function uses for a template it cannot place on
/// either team, and Realm-core's only outcome, since it carries no Character rows at all.
fn legacy_character_house(ctx: &ReducerContext, recipient_guid: u64) -> u32 {
    match crate::helpers::character_by_guid(ctx, recipient_guid) {
        Some(character)
            if lyracore_shared::faction::team_for_race(character.race)
                == lyracore_shared::faction::TEAM_HORDE =>
        {
            6
        }
        Some(_) => 1,
        None => 7,
    }
}

/// Family name `repair_legacy_auction_mail` stamps in `game_import_meta` once it has run on a
/// database, so a later publish's repair pass does not re-scan mail a player wrote after the
/// re-tag — including a letter that happens to name one of [`LEGACY_AUCTION_SUBJECTS`].
const LEGACY_AUCTION_MAIL_REPAIR_FAMILY: &str = "repair_legacy_auction_mail";

/// Re-tag auction mail written before the vanilla Auction Mail format shipped to the vanilla
/// `AuctionHouse` sender and machine subject, once per database. A legacy row still reads as
/// `Character` mail: once the Mail Timer is live, its expiry returns an already-settled "Auction
/// won" to the seller, who was already paid — cmangos deletes a Character mail's item at expiry
/// only when it is unreturned (`cm:ObjectMgr.cpp:6188-6232`), and a legacy Auction Mail was never
/// meant to be returnable at all. Re-tagging moves it out from under that rule, the same way
/// `mail::plan_return` already refuses Return on any current Auction Mail.
///
/// Runs at most once: a marker in `game_import_meta` (`LEGACY_AUCTION_MAIL_REPAIR_FAMILY`) records
/// that this database has already been swept, so a later publish's repair pass leaves every mail
/// alone, including a real player letter that happens to match one of the legacy subjects. Within
/// that one run, [`legacy_mail_matches_shape`] and [`legacy_repair_authorization`] are the two
/// safeguards for every row already on the table: a row converts only if its sender, cod, body,
/// item and money exactly match what the old code produced, and — for "Auction won" and "Auction
/// sold", which carry no tell of their own — a durable listing receipt backs its claim. Returns
/// `(converted, unmapped)`; a nonzero `unmapped` is an auction-looking row this repair left alone,
/// worth a human look.
pub(crate) fn repair_legacy_auction_mail(ctx: &ReducerContext) -> (u64, u64) {
    if ctx
        .db
        .game_import_meta()
        .family()
        .find(LEGACY_AUCTION_MAIL_REPAIR_FAMILY.to_string())
        .is_some()
    {
        return (0, 0);
    }
    let mails = ctx.db.game_mail();
    let candidates: Vec<crate::Mail> = mails
        .iter()
        .filter(|m| {
            m.sender_kind == lyracore_shared::mail::SENDER_KIND_CHARACTER
                && m.subject.starts_with("Auction ")
        })
        .collect();
    let mut converted = 0u64;
    let mut unmapped = 0u64;
    for mut mail in candidates {
        let Some(action) = legacy_auction_action(&mail.subject) else {
            unmapped += 1;
            continue;
        };
        if !legacy_mail_matches_shape(&mail) {
            unmapped += 1;
            continue;
        }
        let Some(target) = legacy_repair_authorization(ctx, &mail, action) else {
            unmapped += 1;
            continue;
        };
        let (sender_kind, sender_guid, sender_entry) =
            MailSender::AuctionHouse(target.house).columns();
        mail.sender_kind = sender_kind;
        mail.sender_guid = sender_guid;
        mail.sender_entry = sender_entry;
        mail.subject = auction_mail_subject(target.item_entry, target.random_property_id, action);
        mail.check_flags = CHECK_MASK_COPIED;
        mails.id().update(mail);
        converted += 1;
    }
    crate::import_meta::stamp(ctx, LEGACY_AUCTION_MAIL_REPAIR_FAMILY, "", "", converted);
    if unmapped > 0 {
        spacetimedb::log::warn!(
            "repair_legacy_auction_mail: {unmapped} auction-looking mail row(s) did not match a \
             known legacy shape and were left unchanged"
        );
    }
    spacetimedb::log::info!(
        "repair_legacy_auction_mail: re-tagged {converted} legacy auction mail row(s) to the \
         AuctionHouse sender"
    );
    (converted, unmapped)
}

fn prepare_listing(
    item: Option<&ListingItem>,
    seller_guid: u64,
    seller_money: u32,
    terms: ListingTerms,
    house: AuctionHousePolicy,
) -> Result<u32, AuctionRefusal> {
    let Some(item) = item else {
        return Err(AuctionRefusal::ItemNotFound);
    };
    if item.owner_guid != seller_guid
        || item.slot < FIRST_BACKPACK_SLOT
        || !crate::items::is_carried_slot(item.slot)
        || !item.mailable
        || item.snapshot.stack_count == 0
        || item.snapshot.soulbound
    {
        return Err(AuctionRefusal::ItemNotFound);
    }
    if terms.start_bid == 0 || (terms.buyout != 0 && terms.buyout < terms.start_bid) {
        return Err(AuctionRefusal::InvalidTerms);
    }
    let deposit = listing_deposit(
        item.sell_price,
        item.snapshot.stack_count,
        terms.duration_minutes,
        house.deposit_rate,
    )
    .ok_or(AuctionRefusal::InvalidTerms)?;
    if !listing_proceeds_are_representable(terms, deposit, house.consignment_rate) {
        return Err(AuctionRefusal::InvalidTerms);
    }
    if seller_money < deposit {
        return Err(AuctionRefusal::NotEnoughMoney);
    }
    Ok(deposit)
}

#[cfg(feature = "debug_reducers")]
mod fixtures;

#[cfg(test)]
mod tests;
