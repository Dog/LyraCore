use super::*;

const BUYOUT_FIXTURE_AUCTION_ID: u32 = 509_0050;
const BUYOUT_FIXTURE_OPERATION_ID: u64 = 509_0050;
const BUYOUT_FIXTURE_SELLER_GUID: u64 = 509_0050;
const BUYOUT_FIXTURE_WINNER_GUID: u64 = 509_0051;
const BUYOUT_FIXTURE_DISPLACED_GUID: u64 = 509_0052;
const BUYOUT_FIXTURE_NEW_BID_AUCTION_ID: u32 = 509_0056;
const BUYOUT_FIXTURE_NEW_BID_OPERATION_ID: u64 = 509_0056;
const BUYOUT_FIXTURE_NEW_BID_SELLER_GUID: u64 = 509_0057;
const BUYOUT_FIXTURE_NEW_BID_BIDDER_GUID: u64 = 509_0058;

/// Stage one reserved Auction row for the standalone buyout integration test, plus a second,
/// unbid, no-buyout Auction (`BUYOUT_FIXTURE_NEW_BID_*`) the same test drives through
/// `AuctionBidEffect::RemainActive` to cover the New Bid notice.
#[reducer]
pub fn debug_stage_auction_buyout_fixture(ctx: &ReducerContext) -> Result<(), String> {
    crate::helpers::require_operator(ctx)?;

    ctx.db
        .game_auction_expiry()
        .auction_id()
        .delete(BUYOUT_FIXTURE_AUCTION_ID);
    ctx.db.game_auction().id().delete(BUYOUT_FIXTURE_AUCTION_ID);
    ctx.db
        .game_auction_bid_decision()
        .operation_id()
        .delete(BUYOUT_FIXTURE_OPERATION_ID);
    ctx.db
        .game_auction_expiry()
        .auction_id()
        .delete(BUYOUT_FIXTURE_NEW_BID_AUCTION_ID);
    ctx.db
        .game_auction()
        .id()
        .delete(BUYOUT_FIXTURE_NEW_BID_AUCTION_ID);
    ctx.db
        .game_auction_bid_decision()
        .operation_id()
        .delete(BUYOUT_FIXTURE_NEW_BID_OPERATION_ID);

    let mails = ctx.db.game_mail();
    let notices = ctx.db.game_auction_notice();
    for recipient_guid in [
        BUYOUT_FIXTURE_SELLER_GUID,
        BUYOUT_FIXTURE_WINNER_GUID,
        BUYOUT_FIXTURE_DISPLACED_GUID,
        BUYOUT_FIXTURE_NEW_BID_SELLER_GUID,
        BUYOUT_FIXTURE_NEW_BID_BIDDER_GUID,
    ] {
        let stale_mail: Vec<u64> = mails
            .by_recipient()
            .filter(&recipient_guid)
            .filter(|mail| mail.sender_kind == lyracore_shared::mail::SENDER_KIND_AUCTION)
            .map(|mail| mail.id)
            .collect();
        for id in stale_mail {
            crate::mail::delete_mail(ctx, id);
        }
        let stale_notices: Vec<u64> = notices
            .by_recipient()
            .filter(&recipient_guid)
            .map(|notice| notice.id)
            .collect();
        for id in stale_notices {
            notices.id().delete(id);
        }
    }

    let expires_micros = ctx
        .timestamp
        .to_micros_since_unix_epoch()
        .checked_add(3_600_000_000)
        .ok_or_else(|| "auction buyout fixture expiry overflow".to_string())?;
    let expires_at = Timestamp::from_micros_since_unix_epoch(expires_micros);
    ctx.db.game_auction().insert(Auction {
        id: BUYOUT_FIXTURE_AUCTION_ID,
        listing_operation_id: BUYOUT_FIXTURE_OPERATION_ID - 1,
        house: 1,
        owner_guid: BUYOUT_FIXTURE_SELLER_GUID,
        item_guid: 509_0053,
        item_entry: 509_0050,
        item_stack_count: 2,
        item_durability: 17,
        item_enchant_id: 9,
        item_soulbound: false,
        random_property_id: 117,
        start_bid: 100,
        buyout: 500,
        highest_bidder_guid: BUYOUT_FIXTURE_DISPLACED_GUID,
        highest_bid: 201,
        deposit: 10,
        created_at: ctx.timestamp,
        expires_at,
        revision: 3,
        deposit_rate: 5,
        consignment_rate: 5,
        item_text_id: 0,
    });
    ctx.db.game_auction_expiry().insert(AuctionExpiry {
        scheduled_id: 0,
        scheduled_at: ScheduleAt::Time(expires_at),
        auction_id: BUYOUT_FIXTURE_AUCTION_ID,
    });

    // No buyout term, so any accepted offer is RemainActive — an ordinary raise, never a
    // settlement — and no prior bidder, so the decision carries no displaced bidder either.
    let new_bid_expires_at =
        Timestamp::from_micros_since_unix_epoch(expires_micros.saturating_add(1));
    ctx.db.game_auction().insert(Auction {
        id: BUYOUT_FIXTURE_NEW_BID_AUCTION_ID,
        listing_operation_id: BUYOUT_FIXTURE_NEW_BID_OPERATION_ID - 1,
        house: 1,
        owner_guid: BUYOUT_FIXTURE_NEW_BID_SELLER_GUID,
        item_guid: 509_0059,
        item_entry: BUYOUT_FIXTURE_NEW_BID_AUCTION_ID,
        item_stack_count: 1,
        item_durability: 0,
        item_enchant_id: 0,
        item_soulbound: false,
        random_property_id: 0,
        start_bid: 50,
        buyout: 0,
        highest_bidder_guid: 0,
        highest_bid: 0,
        deposit: 5,
        created_at: ctx.timestamp,
        expires_at: new_bid_expires_at,
        revision: 0,
        deposit_rate: 5,
        consignment_rate: 5,
        item_text_id: 0,
    });
    ctx.db.game_auction_expiry().insert(AuctionExpiry {
        scheduled_id: 0,
        scheduled_at: ScheduleAt::Time(new_bid_expires_at),
        auction_id: BUYOUT_FIXTURE_NEW_BID_AUCTION_ID,
    });
    Ok(())
}

fn auction_fixture_mail(
    ctx: &ReducerContext,
    recipient_guid: u64,
    house: u32,
    item_entry: u32,
    random_property_id: u32,
    action: AuctionMailAction,
) -> Result<crate::Mail, String> {
    let subject = auction_mail_subject(item_entry, random_property_id, action);
    let mut matches: Vec<_> = ctx
        .db
        .game_mail()
        .by_recipient()
        .filter(&recipient_guid)
        .filter(|mail| mail.subject == subject && mail.sender() == MailSender::AuctionHouse(house))
        .collect();
    if matches.len() != 1 {
        return Err(format!(
            "expected one {subject:?} mail for {recipient_guid}, found {}",
            matches.len()
        ));
    }
    Ok(matches.remove(0))
}

/// The exactly-one notice of `kind` for `recipient_guid`, verifying every wire field the
/// gateway relays.
#[allow(clippy::too_many_arguments)]
fn auction_fixture_notice(
    ctx: &ReducerContext,
    recipient_guid: u64,
    kind: u8,
    house: u32,
    auction_id: u32,
    item_entry: u32,
    random_property_id: u32,
    bid: u32,
    out_bid: u32,
    bidder_guid: u64,
) -> Result<(), String> {
    let matches: Vec<_> = ctx
        .db
        .game_auction_notice()
        .by_recipient()
        .filter(&recipient_guid)
        .filter(|notice| notice.kind == kind)
        .collect();
    if matches.len() != 1 {
        return Err(format!(
            "expected one kind {kind} notice for {recipient_guid}, found {}",
            matches.len()
        ));
    }
    let notice = &matches[0];
    if notice.house != house
        || notice.auction_id != auction_id
        || notice.item_entry != item_entry
        || notice.random_property_id != random_property_id
        || notice.bid != bid
        || notice.out_bid != out_bid
        || notice.bidder_guid != bidder_guid
    {
        return Err(format!("kind {kind} notice for {recipient_guid} changed"));
    }
    Ok(())
}

/// Verify the real realm reducer committed exact settlement rows in a prior transaction, including
/// every Auction Notice. Auction Notices are a one-shot, TTL-reaped relay (see `gc.rs`); the
/// durable test disarms the reaper schedule before staging this fixture, so nothing claims a
/// notice row while this test runs and this check is safe to call whenever the test wants it,
/// regardless of what ran before it.
#[reducer]
pub fn debug_verify_auction_buyout_fixture(ctx: &ReducerContext) -> Result<(), String> {
    crate::helpers::require_operator(ctx)?;
    if ctx
        .db
        .game_auction()
        .id()
        .find(BUYOUT_FIXTURE_AUCTION_ID)
        .is_some()
        || ctx
            .db
            .game_auction_expiry()
            .auction_id()
            .find(BUYOUT_FIXTURE_AUCTION_ID)
            .is_some()
    {
        return Err("settled Auction or expiry schedule is still active".to_string());
    }

    let decision = ctx
        .db
        .game_auction_bid_decision()
        .operation_id()
        .find(BUYOUT_FIXTURE_OPERATION_ID)
        .ok_or_else(|| "buyout decision was not committed".to_string())?;
    if decision.bidder_guid != BUYOUT_FIXTURE_WINNER_GUID
        || decision.auction_id != BUYOUT_FIXTURE_AUCTION_ID
        || decision.offer != 900
        || decision.outcome != BID_ACCEPTED
        || decision.revision != 0
        || decision.result_bidder_guid != BUYOUT_FIXTURE_DISPLACED_GUID
        || decision.result_bid != 201
        || decision.accepted_price != 500
    {
        return Err("buyout decision payload changed".to_string());
    }

    const HOUSE: u32 = 1;
    let item = crate::items::ItemSnapshot {
        entry: 509_0050,
        stack_count: 2,
        durability: 17,
        enchant_id: 9,
        soulbound: false,
        random_property_id: 117,
        item_text_id: 0,
    };

    let refund = auction_fixture_mail(
        ctx,
        BUYOUT_FIXTURE_DISPLACED_GUID,
        HOUSE,
        item.entry,
        item.random_property_id,
        AuctionMailAction::Outbid,
    )?;
    if refund.money != 201
        || refund.cod != 0
        || refund.was_read
        || refund.check_flags != CHECK_MASK_COPIED
        || !refund.body.is_empty()
        || !refund.snapshot().is_empty()
    {
        return Err("displaced-bidder refund mail changed".to_string());
    }

    let winner = auction_fixture_mail(
        ctx,
        BUYOUT_FIXTURE_WINNER_GUID,
        HOUSE,
        item.entry,
        item.random_property_id,
        AuctionMailAction::Won,
    )?;
    if winner.money != 0
        || winner.cod != 0
        || winner.was_read
        || winner.check_flags != CHECK_MASK_COPIED
        || winner.body != format!("{:>16x}:{}:{}", BUYOUT_FIXTURE_SELLER_GUID, 500, 500)
        || winner.snapshot() != item
    {
        return Err("winner item mail changed".to_string());
    }

    let seller = auction_fixture_mail(
        ctx,
        BUYOUT_FIXTURE_SELLER_GUID,
        HOUSE,
        item.entry,
        item.random_property_id,
        AuctionMailAction::Successful,
    )?;
    if seller.money != 485
        || seller.cod != 0
        || seller.was_read
        || seller.check_flags != CHECK_MASK_COPIED
        || seller.body
            != format!(
                "{:>16x}:{}:{}:{}:{}",
                BUYOUT_FIXTURE_WINNER_GUID, 500, 500, 10, 25
            )
        || !seller.snapshot().is_empty()
    {
        return Err("seller proceeds mail changed".to_string());
    }

    auction_fixture_notice(
        ctx,
        BUYOUT_FIXTURE_DISPLACED_GUID,
        auction_notice::OUTBID,
        HOUSE,
        BUYOUT_FIXTURE_AUCTION_ID,
        item.entry,
        item.random_property_id,
        201,
        11, // vanilla minimum raise on a 201 bid: 5% rounded up
        BUYOUT_FIXTURE_DISPLACED_GUID,
    )?;
    auction_fixture_notice(
        ctx,
        BUYOUT_FIXTURE_WINNER_GUID,
        auction_notice::WON,
        HOUSE,
        BUYOUT_FIXTURE_AUCTION_ID,
        item.entry,
        item.random_property_id,
        0,
        25, // vanilla minimum raise on a 500 bid: 5% rounded up
        BUYOUT_FIXTURE_WINNER_GUID,
    )?;
    auction_fixture_notice(
        ctx,
        BUYOUT_FIXTURE_SELLER_GUID,
        auction_notice::SOLD,
        HOUSE,
        BUYOUT_FIXTURE_AUCTION_ID,
        item.entry,
        item.random_property_id,
        500,
        25, // vanilla minimum raise on a 500 bid: 5% rounded up
        0,
    )?;

    // RemainActive coverage: an ordinary raise on a fresh listing settles nothing and displaces
    // nobody, so it must record the new bid, mail nobody, and write exactly one New Bid notice to
    // the owner.
    let new_bid_auction = ctx
        .db
        .game_auction()
        .id()
        .find(BUYOUT_FIXTURE_NEW_BID_AUCTION_ID)
        .ok_or_else(|| "the RemainActive Auction was settled or expired".to_string())?;
    if new_bid_auction.highest_bidder_guid != BUYOUT_FIXTURE_NEW_BID_BIDDER_GUID
        || new_bid_auction.highest_bid != 60
        || new_bid_auction.revision != 1
    {
        return Err("the RemainActive Auction did not record the new bid".to_string());
    }
    if ctx
        .db
        .game_mail()
        .by_recipient()
        .filter(&BUYOUT_FIXTURE_NEW_BID_BIDDER_GUID)
        .any(|mail| mail.sender_kind == lyracore_shared::mail::SENDER_KIND_AUCTION)
    {
        return Err(
            "a fresh bid with no displaced bidder must not mail its own bidder".to_string(),
        );
    }
    auction_fixture_notice(
        ctx,
        BUYOUT_FIXTURE_NEW_BID_SELLER_GUID,
        auction_notice::NEW_BID,
        HOUSE,
        BUYOUT_FIXTURE_NEW_BID_AUCTION_ID,
        BUYOUT_FIXTURE_NEW_BID_AUCTION_ID,
        0,
        60,
        3, // vanilla minimum raise on a 60 bid: 5% rounded up
        BUYOUT_FIXTURE_NEW_BID_BIDDER_GUID,
    )?;
    Ok(())
}

/// The listing with a bid, the unbid listing, and the listing whose cut the test offers wrong.
const CANCEL_FIXTURE_AUCTION_IDS: [u32; 3] = [509_0086, 509_0088, 509_0089];
const CANCEL_FIXTURE_BIDDER_GUID: u64 = 509_0087;
/// Every operation id the Cancellation test may use, cleared before each staging.
const CANCEL_FIXTURE_OPERATION_IDS: std::ops::RangeInclusive<u64> = 509_0086..=509_0095;
/// Blackwater's faction template (`cm:AuctionHouseMgr.cpp:461-518`), so the auctioneer serves the
/// neutral house 7.
const CANCEL_FIXTURE_FACTION_TEMPLATE: u32 = 120;

/// Stage three listings of `seller_guid` in house 7 at a consignment rate of 15, each with its
/// expiry row: 509_0086 carries a bid of 1000 by 509_0087 (cut 150), 509_0088 has no bid (cut 0),
/// and 509_0089 carries the same bid. A nonzero `auctioneer_guid` becomes an auctioneer of house 7,
/// and the fixture imports house 7 and its faction template when the database has none.
#[reducer]
pub fn debug_stage_auction_cancel_fixture(
    ctx: &ReducerContext,
    seller_guid: u64,
    auctioneer_guid: u64,
) -> Result<(), String> {
    crate::helpers::require_operator(ctx)?;
    const HOUSE: u32 = 7;

    for auction_id in CANCEL_FIXTURE_AUCTION_IDS {
        ctx.db.game_auction_expiry().auction_id().delete(auction_id);
        ctx.db.game_auction().id().delete(auction_id);
    }
    for operation_id in CANCEL_FIXTURE_OPERATION_IDS {
        ctx.db
            .game_auction_bid_hold()
            .operation_id()
            .delete(operation_id);
        ctx.db
            .game_auction_bid_decision()
            .operation_id()
            .delete(operation_id);
    }
    let notices = ctx.db.game_auction_notice();
    for recipient_guid in [seller_guid, CANCEL_FIXTURE_BIDDER_GUID] {
        let stale_mail: Vec<u64> = ctx
            .db
            .game_mail()
            .by_recipient()
            .filter(&recipient_guid)
            .filter(|mail| mail.sender_kind == lyracore_shared::mail::SENDER_KIND_AUCTION)
            .map(|mail| mail.id)
            .collect();
        for id in stale_mail {
            crate::mail::delete_mail(ctx, id);
        }
        let stale_notices: Vec<u64> = notices
            .by_recipient()
            .filter(&recipient_guid)
            .map(|notice| notice.id)
            .collect();
        for id in stale_notices {
            notices.id().delete(id);
        }
    }

    if ctx.db.game_auction_house().id().find(HOUSE).is_none() {
        ctx.db.game_auction_house().insert(AuctionHouseDefinition {
            id: HOUSE,
            faction: 0,
            deposit_rate: 25,
            consignment_rate: 15,
            name: "Blackwater Auction House".to_string(),
        });
    }
    if auctioneer_guid != 0 {
        if ctx
            .db
            .game_faction_template()
            .id()
            .find(CANCEL_FIXTURE_FACTION_TEMPLATE)
            .is_none()
        {
            ctx.db
                .game_faction_template()
                .insert(crate::faction::FactionTemplate {
                    id: CANCEL_FIXTURE_FACTION_TEMPLATE,
                    faction: 0,
                    faction_group: 0,
                    friend_group: 0,
                    enemy_group: 0,
                    enemy_0: 0,
                    enemy_1: 0,
                    enemy_2: 0,
                    enemy_3: 0,
                    friend_0: 0,
                    friend_1: 0,
                    friend_2: 0,
                    friend_3: 0,
                });
        }
        let mut auctioneer = ctx
            .db
            .game_world_entity()
            .guid()
            .find(auctioneer_guid)
            .ok_or_else(|| format!("auctioneer {auctioneer_guid} is not in the world"))?;
        auctioneer.faction_template = CANCEL_FIXTURE_FACTION_TEMPLATE;
        auctioneer.npc_flags |= lyracore_shared::constants::npc_flags::AUCTIONEER;
        ctx.db.game_world_entity().guid().update(auctioneer);
    }

    let expires_micros = ctx
        .timestamp
        .to_micros_since_unix_epoch()
        .checked_add(3_600_000_000)
        .ok_or_else(|| "auction Cancellation fixture expiry overflow".to_string())?;
    let expires_at = Timestamp::from_micros_since_unix_epoch(expires_micros);
    for (auction_id, highest_bidder_guid, highest_bid) in [
        (
            CANCEL_FIXTURE_AUCTION_IDS[0],
            CANCEL_FIXTURE_BIDDER_GUID,
            1_000,
        ),
        (CANCEL_FIXTURE_AUCTION_IDS[1], 0, 0),
        (
            CANCEL_FIXTURE_AUCTION_IDS[2],
            CANCEL_FIXTURE_BIDDER_GUID,
            1_000,
        ),
    ] {
        ctx.db.game_auction().insert(Auction {
            id: auction_id,
            listing_operation_id: u64::from(auction_id),
            house: HOUSE,
            owner_guid: seller_guid,
            item_guid: u64::from(auction_id),
            item_entry: auction_id,
            item_stack_count: 3,
            item_durability: 17,
            item_enchant_id: 9,
            item_soulbound: false,
            random_property_id: 117,
            start_bid: 500,
            buyout: 0,
            highest_bidder_guid,
            highest_bid,
            deposit: 40,
            created_at: ctx.timestamp,
            expires_at,
            revision: u64::from(highest_bid != 0),
            deposit_rate: 25,
            consignment_rate: 15,
            item_text_id: 0,
        });
        ctx.db.game_auction_expiry().insert(AuctionExpiry {
            scheduled_id: 0,
            scheduled_at: ScheduleAt::Time(expires_at),
            auction_id,
        });
    }
    Ok(())
}

const EXPIRY_FIXTURE_AUCTION_ID: u32 = 509_0060;
const EXPIRY_FIXTURE_OPERATION_ID: u64 = 509_0060;
const EXPIRY_FIXTURE_SELLER_GUID: u64 = 509_0060;
const EXPIRY_FIXTURE_WINNER_GUID: u64 = 509_0061;
const EXPIRY_FIXTURE_ITEM: crate::items::ItemSnapshot = crate::items::ItemSnapshot {
    entry: 509_0060,
    stack_count: 2,
    durability: 17,
    enchant_id: 9,
    soulbound: false,
    random_property_id: 117,
    item_text_id: 0,
};
const EXPIRY_FIXTURE_UNSOLD_AUCTION_ID: u32 = 509_0065;
const EXPIRY_FIXTURE_UNSOLD_OPERATION_ID: u64 = 509_0065;
const EXPIRY_FIXTURE_UNSOLD_SELLER_GUID: u64 = 509_0065;
const EXPIRY_FIXTURE_UNSOLD_ITEM: crate::items::ItemSnapshot = crate::items::ItemSnapshot {
    entry: 509_0066,
    stack_count: 1,
    durability: 0,
    enchant_id: 0,
    soulbound: false,
    random_property_id: 0,
    item_text_id: 0,
};

/// Stage a valid winning-bid Auction whose one-shot schedule fires shortly after this
/// transaction, plus a second, unbid Auction (`EXPIRY_FIXTURE_UNSOLD_*`) whose expiry has no
/// winner, to cover the Expired notice.
#[reducer]
pub fn debug_stage_auction_expiry_fixture(ctx: &ReducerContext) -> Result<(), String> {
    crate::helpers::require_operator(ctx)?;

    ctx.db
        .game_auction_expiry()
        .auction_id()
        .delete(EXPIRY_FIXTURE_AUCTION_ID);
    ctx.db.game_auction().id().delete(EXPIRY_FIXTURE_AUCTION_ID);
    ctx.db
        .game_auction_operation_receipt()
        .operation_id()
        .delete(EXPIRY_FIXTURE_OPERATION_ID);
    ctx.db
        .game_auction_expiry()
        .auction_id()
        .delete(EXPIRY_FIXTURE_UNSOLD_AUCTION_ID);
    ctx.db
        .game_auction()
        .id()
        .delete(EXPIRY_FIXTURE_UNSOLD_AUCTION_ID);
    ctx.db
        .game_auction_operation_receipt()
        .operation_id()
        .delete(EXPIRY_FIXTURE_UNSOLD_OPERATION_ID);

    let mails = ctx.db.game_mail();
    let notices = ctx.db.game_auction_notice();
    for recipient_guid in [
        EXPIRY_FIXTURE_SELLER_GUID,
        EXPIRY_FIXTURE_WINNER_GUID,
        EXPIRY_FIXTURE_UNSOLD_SELLER_GUID,
    ] {
        let stale_mail: Vec<u64> = mails
            .by_recipient()
            .filter(&recipient_guid)
            .filter(|mail| mail.sender_kind == lyracore_shared::mail::SENDER_KIND_AUCTION)
            .map(|mail| mail.id)
            .collect();
        for id in stale_mail {
            crate::mail::delete_mail(ctx, id);
        }
        let stale_notices: Vec<u64> = notices
            .by_recipient()
            .filter(&recipient_guid)
            .map(|notice| notice.id)
            .collect();
        for id in stale_notices {
            notices.id().delete(id);
        }
    }

    let expires_micros = ctx
        .timestamp
        .to_micros_since_unix_epoch()
        .checked_add(500_000)
        .ok_or_else(|| "auction expiry fixture deadline overflow".to_string())?;
    let created_micros = expires_micros
        .checked_sub(28_800_000_000)
        .ok_or_else(|| "auction expiry fixture creation time underflow".to_string())?;
    let expires_at = Timestamp::from_micros_since_unix_epoch(expires_micros);
    let created_at = Timestamp::from_micros_since_unix_epoch(created_micros);
    ctx.db
        .game_auction_operation_receipt()
        .insert(AuctionOperationReceipt {
            operation_id: EXPIRY_FIXTURE_OPERATION_ID,
            auction_id: EXPIRY_FIXTURE_AUCTION_ID,
            actor_guid: EXPIRY_FIXTURE_SELLER_GUID,
            item_guid: 509_0063,
            item_entry: EXPIRY_FIXTURE_ITEM.entry,
            item_stack_count: EXPIRY_FIXTURE_ITEM.stack_count,
            item_durability: EXPIRY_FIXTURE_ITEM.durability,
            item_enchant_id: EXPIRY_FIXTURE_ITEM.enchant_id,
            item_soulbound: EXPIRY_FIXTURE_ITEM.soulbound,
            random_property_id: EXPIRY_FIXTURE_ITEM.random_property_id,
            start_bid: 100,
            buyout: 500,
            duration_minutes: 480,
            deposit: 10,
            created_micros,
            expires_micros,
            house: 1,
            deposit_rate: 5,
            consignment_rate: 5,
            item_text_id: 0,
        });
    ctx.db.game_auction().insert(Auction {
        id: EXPIRY_FIXTURE_AUCTION_ID,
        listing_operation_id: EXPIRY_FIXTURE_OPERATION_ID,
        house: 1,
        owner_guid: EXPIRY_FIXTURE_SELLER_GUID,
        item_guid: 509_0063,
        item_entry: EXPIRY_FIXTURE_ITEM.entry,
        item_stack_count: EXPIRY_FIXTURE_ITEM.stack_count,
        item_durability: EXPIRY_FIXTURE_ITEM.durability,
        item_enchant_id: EXPIRY_FIXTURE_ITEM.enchant_id,
        item_soulbound: EXPIRY_FIXTURE_ITEM.soulbound,
        random_property_id: EXPIRY_FIXTURE_ITEM.random_property_id,
        start_bid: 100,
        buyout: 500,
        highest_bidder_guid: EXPIRY_FIXTURE_WINNER_GUID,
        highest_bid: 201,
        deposit: 10,
        created_at,
        expires_at,
        revision: 3,
        deposit_rate: 5,
        consignment_rate: 5,
        item_text_id: 0,
    });
    ctx.db.game_auction_expiry().insert(AuctionExpiry {
        scheduled_id: 0,
        scheduled_at: ScheduleAt::Time(expires_at),
        auction_id: EXPIRY_FIXTURE_AUCTION_ID,
    });

    // Unsold: no bidder ever placed an offer, so expiry returns the item and mails the owner an
    // Expired notice rather than settling a sale.
    let unsold_expires_at = Timestamp::from_micros_since_unix_epoch(expires_micros + 1);
    ctx.db
        .game_auction_operation_receipt()
        .insert(AuctionOperationReceipt {
            operation_id: EXPIRY_FIXTURE_UNSOLD_OPERATION_ID,
            auction_id: EXPIRY_FIXTURE_UNSOLD_AUCTION_ID,
            actor_guid: EXPIRY_FIXTURE_UNSOLD_SELLER_GUID,
            item_guid: 509_0068,
            item_entry: EXPIRY_FIXTURE_UNSOLD_ITEM.entry,
            item_stack_count: EXPIRY_FIXTURE_UNSOLD_ITEM.stack_count,
            item_durability: EXPIRY_FIXTURE_UNSOLD_ITEM.durability,
            item_enchant_id: EXPIRY_FIXTURE_UNSOLD_ITEM.enchant_id,
            item_soulbound: EXPIRY_FIXTURE_UNSOLD_ITEM.soulbound,
            random_property_id: EXPIRY_FIXTURE_UNSOLD_ITEM.random_property_id,
            start_bid: 100,
            buyout: 500,
            duration_minutes: 480,
            deposit: 10,
            created_micros,
            expires_micros: expires_micros + 1,
            house: 1,
            deposit_rate: 5,
            consignment_rate: 5,
            item_text_id: 0,
        });
    ctx.db.game_auction().insert(Auction {
        id: EXPIRY_FIXTURE_UNSOLD_AUCTION_ID,
        listing_operation_id: EXPIRY_FIXTURE_UNSOLD_OPERATION_ID,
        house: 1,
        owner_guid: EXPIRY_FIXTURE_UNSOLD_SELLER_GUID,
        item_guid: 509_0068,
        item_entry: EXPIRY_FIXTURE_UNSOLD_ITEM.entry,
        item_stack_count: EXPIRY_FIXTURE_UNSOLD_ITEM.stack_count,
        item_durability: EXPIRY_FIXTURE_UNSOLD_ITEM.durability,
        item_enchant_id: EXPIRY_FIXTURE_UNSOLD_ITEM.enchant_id,
        item_soulbound: EXPIRY_FIXTURE_UNSOLD_ITEM.soulbound,
        random_property_id: EXPIRY_FIXTURE_UNSOLD_ITEM.random_property_id,
        start_bid: 100,
        buyout: 500,
        highest_bidder_guid: 0,
        highest_bid: 0,
        deposit: 10,
        created_at,
        expires_at: unsold_expires_at,
        revision: 0,
        deposit_rate: 5,
        consignment_rate: 5,
        item_text_id: 0,
    });
    ctx.db.game_auction_expiry().insert(AuctionExpiry {
        scheduled_id: 0,
        scheduled_at: ScheduleAt::Time(unsold_expires_at),
        auction_id: EXPIRY_FIXTURE_UNSOLD_AUCTION_ID,
    });
    Ok(())
}

/// Re-drive the callback body after settlement to prove the missing Auction is a durable no-op.
#[reducer]
pub fn debug_replay_auction_expiry_fixture(ctx: &ReducerContext) -> Result<(), String> {
    crate::helpers::require_operator(ctx)?;
    expire_active(&mut CtxExpiry { ctx }, EXPIRY_FIXTURE_AUCTION_ID)?;
    expire_active(&mut CtxExpiry { ctx }, EXPIRY_FIXTURE_UNSOLD_AUCTION_ID)
}

/// Verify the scheduler committed the exact bid-expiry mail, removed only active state, and fired
/// every Auction Notice. Auction Notices are a one-shot, TTL-reaped relay (see `gc.rs`); the
/// durable test disarms the reaper schedule before staging this fixture, so nothing claims a
/// notice row while this test runs and this check is safe to call whenever the test wants it,
/// regardless of what ran before it.
#[reducer]
pub fn debug_verify_auction_expiry_fixture(ctx: &ReducerContext) -> Result<(), String> {
    crate::helpers::require_operator(ctx)?;
    if ctx
        .db
        .game_auction()
        .id()
        .find(EXPIRY_FIXTURE_AUCTION_ID)
        .is_some()
        || ctx
            .db
            .game_auction_expiry()
            .auction_id()
            .find(EXPIRY_FIXTURE_AUCTION_ID)
            .is_some()
        || ctx
            .db
            .game_auction()
            .id()
            .find(EXPIRY_FIXTURE_UNSOLD_AUCTION_ID)
            .is_some()
        || ctx
            .db
            .game_auction_expiry()
            .auction_id()
            .find(EXPIRY_FIXTURE_UNSOLD_AUCTION_ID)
            .is_some()
    {
        return Err("expired Auction or schedule is still active".to_string());
    }
    if ctx
        .db
        .game_auction_operation_receipt()
        .operation_id()
        .find(EXPIRY_FIXTURE_OPERATION_ID)
        .is_none()
        || ctx
            .db
            .game_auction_operation_receipt()
            .operation_id()
            .find(EXPIRY_FIXTURE_UNSOLD_OPERATION_ID)
            .is_none()
    {
        return Err("expiry removed the durable listing receipt".to_string());
    }

    const HOUSE: u32 = 1;
    let winner = auction_fixture_mail(
        ctx,
        EXPIRY_FIXTURE_WINNER_GUID,
        HOUSE,
        EXPIRY_FIXTURE_ITEM.entry,
        EXPIRY_FIXTURE_ITEM.random_property_id,
        AuctionMailAction::Won,
    )?;
    if winner.money != 0
        || winner.cod != 0
        || winner.was_read
        || winner.check_flags != CHECK_MASK_COPIED
        || winner.body != format!("{:>16x}:{}:{}", EXPIRY_FIXTURE_SELLER_GUID, 201, 500)
        || winner.snapshot() != EXPIRY_FIXTURE_ITEM
    {
        return Err("expiry winner item mail changed".to_string());
    }

    let seller = auction_fixture_mail(
        ctx,
        EXPIRY_FIXTURE_SELLER_GUID,
        HOUSE,
        EXPIRY_FIXTURE_ITEM.entry,
        EXPIRY_FIXTURE_ITEM.random_property_id,
        AuctionMailAction::Successful,
    )?;
    if seller.money != 201
        || seller.cod != 0
        || seller.was_read
        || seller.check_flags != CHECK_MASK_COPIED
        || seller.body
            != format!(
                "{:>16x}:{}:{}:{}:{}",
                EXPIRY_FIXTURE_WINNER_GUID, 201, 500, 10, 10
            )
        || !seller.snapshot().is_empty()
    {
        return Err("expiry seller proceeds mail changed".to_string());
    }

    // Unsold coverage: no bidder means expiry returns the item, not a Won/Sold pair.
    let returned = auction_fixture_mail(
        ctx,
        EXPIRY_FIXTURE_UNSOLD_SELLER_GUID,
        HOUSE,
        EXPIRY_FIXTURE_UNSOLD_ITEM.entry,
        EXPIRY_FIXTURE_UNSOLD_ITEM.random_property_id,
        AuctionMailAction::Expired,
    )?;
    if returned.money != 0
        || returned.cod != 0
        || returned.was_read
        || returned.check_flags != CHECK_MASK_COPIED
        || !returned.body.is_empty()
        || returned.snapshot() != EXPIRY_FIXTURE_UNSOLD_ITEM
    {
        return Err("unsold expiry return mail changed".to_string());
    }

    auction_fixture_notice(
        ctx,
        EXPIRY_FIXTURE_WINNER_GUID,
        auction_notice::WON,
        HOUSE,
        EXPIRY_FIXTURE_AUCTION_ID,
        EXPIRY_FIXTURE_ITEM.entry,
        EXPIRY_FIXTURE_ITEM.random_property_id,
        0,
        11, // vanilla minimum raise on a 201 bid: 5% rounded up
        EXPIRY_FIXTURE_WINNER_GUID,
    )?;
    auction_fixture_notice(
        ctx,
        EXPIRY_FIXTURE_SELLER_GUID,
        auction_notice::SOLD,
        HOUSE,
        EXPIRY_FIXTURE_AUCTION_ID,
        EXPIRY_FIXTURE_ITEM.entry,
        EXPIRY_FIXTURE_ITEM.random_property_id,
        201,
        11, // vanilla minimum raise on a 201 bid: 5% rounded up
        0,
    )?;
    auction_fixture_notice(
        ctx,
        EXPIRY_FIXTURE_UNSOLD_SELLER_GUID,
        auction_notice::EXPIRED,
        HOUSE,
        EXPIRY_FIXTURE_UNSOLD_AUCTION_ID,
        EXPIRY_FIXTURE_UNSOLD_ITEM.entry,
        EXPIRY_FIXTURE_UNSOLD_ITEM.random_property_id,
        0,
        1, // bid_increment(0) is 0, but expired_notice floors an unsold listing's raise to 1.
        0,
    )?;
    Ok(())
}

const LEGACY_MAIL_FIXTURE_RECIPIENT_GUID: u64 = 509_0070;
const LEGACY_MAIL_FIXTURE_ITEM_ENTRY: u32 = 509_0071;
const LEGACY_MAIL_LOOKALIKE_RECIPIENT_GUID: u64 = 509_0072;
const LEGACY_MAIL_LOOKALIKE_SENDER_GUID: u64 = 509_0073;
const LEGACY_MAIL_LOOKALIKE_ITEM_ENTRY: u32 = 509_0074;
const LEGACY_MAIL_LOOKALIKE_COD: u32 = 250;
// A real letter titled "Auction won" can carry cod == 0 too (nothing stops a player from gifting
// an item for free); the guid-0 look-alike above already covers the COD case, so this covers the
// other one. It has no receipt naming its sender as this item's seller, so it must stay untouched
// even though it clears `legacy_mail_matches_shape`.
const LEGACY_MAIL_NO_COD_LOOKALIKE_RECIPIENT_GUID: u64 = 509_0076;
const LEGACY_MAIL_NO_COD_LOOKALIKE_SENDER_GUID: u64 = 509_0077;
const LEGACY_MAIL_NO_COD_LOOKALIKE_ITEM_ENTRY: u32 = 509_0078;
// The receipt that authorizes the legitimate "Auction won" row above. Its house (6, Horde) is
// deliberately not the neutral fallback (7): Realm-core carries no Character rows, so a converted
// row's house only comes from here if `legacy_repair_authorization` truly reads the receipt
// instead of silently falling back.
const LEGACY_MAIL_WON_RECEIPT_OPERATION_ID: u64 = 509_0075;
const LEGACY_MAIL_WON_RECEIPT_AUCTION_ID: u32 = 509_0075;
const LEGACY_MAIL_WON_RECEIPT_HOUSE: u32 = 6;
// A legitimate "Auction sold" row: proceeds mail with no item attached, so its vanilla subject can
// only name the sold item by borrowing the authorizing receipt's own entry and random property id.
const LEGACY_MAIL_SOLD_FIXTURE_RECIPIENT_GUID: u64 = 509_0079;
const LEGACY_MAIL_SOLD_RECEIPT_OPERATION_ID: u64 = 509_0080;
const LEGACY_MAIL_SOLD_RECEIPT_AUCTION_ID: u32 = 509_0080;
const LEGACY_MAIL_SOLD_RECEIPT_ITEM_ENTRY: u32 = 509_0081;
const LEGACY_MAIL_SOLD_RECEIPT_HOUSE: u32 = 1;
// The same seller has two other receipts on file: a refused listing (the `auction_id == 0`
// sentinel — excluded outright, whatever its proceeds) and a second real listing whose price range
// could never have paid out the Sold mail's money (excluded by `receipt_could_pay_out`). Both use a
// house the repair must never select, so picking either fails the test loudly instead of quietly.
const LEGACY_MAIL_REFUSED_RECEIPT_OPERATION_ID: u64 = 509_0082;
const LEGACY_MAIL_REFUSED_RECEIPT_ITEM_ENTRY: u32 = 509_0083;
const LEGACY_MAIL_DECOY_RECEIPT_OPERATION_ID: u64 = 509_0084;
const LEGACY_MAIL_DECOY_RECEIPT_AUCTION_ID: u32 = 509_0084;
const LEGACY_MAIL_DECOY_RECEIPT_ITEM_ENTRY: u32 = 509_0085;
const LEGACY_MAIL_WRONG_RECEIPT_HOUSE: u32 = 7;

/// Stage `Character`-sender rows covering every outcome `repair_legacy_auction_mail` must tell
/// apart: a legitimate "Auction won" row backed by a listing receipt, a legitimate "Auction sold"
/// row backed by one of three receipts on the same seller (a refused listing, a real listing that
/// could not have paid this exact price, and the real listing that did), and two look-alikes a real
/// player could send today — one with a cash-on-delivery price (the shape check alone rules this
/// out), one without (only the missing receipt rules this out). Proves `repair_legacy_auction_mail`
/// against real rows on a real database: it must convert the two legitimate rows, picking the one
/// true receipt for Sold out of three candidates, and leave both look-alikes alone.
#[reducer]
pub fn debug_stage_legacy_auction_mail_fixture(ctx: &ReducerContext) -> Result<(), String> {
    crate::helpers::require_operator(ctx)?;
    let mails = ctx.db.game_mail();
    let stale: Vec<u64> = mails
        .by_recipient()
        .filter(&LEGACY_MAIL_FIXTURE_RECIPIENT_GUID)
        .chain(
            mails
                .by_recipient()
                .filter(&LEGACY_MAIL_LOOKALIKE_RECIPIENT_GUID),
        )
        .chain(
            mails
                .by_recipient()
                .filter(&LEGACY_MAIL_NO_COD_LOOKALIKE_RECIPIENT_GUID),
        )
        .chain(
            mails
                .by_recipient()
                .filter(&LEGACY_MAIL_SOLD_FIXTURE_RECIPIENT_GUID),
        )
        .map(|mail| mail.id)
        .collect();
    for id in stale {
        crate::mail::delete_mail(ctx, id);
    }
    let receipts = ctx.db.game_auction_operation_receipt();
    receipts
        .operation_id()
        .delete(LEGACY_MAIL_WON_RECEIPT_OPERATION_ID);
    receipts
        .operation_id()
        .delete(LEGACY_MAIL_SOLD_RECEIPT_OPERATION_ID);
    receipts
        .operation_id()
        .delete(LEGACY_MAIL_REFUSED_RECEIPT_OPERATION_ID);
    receipts
        .operation_id()
        .delete(LEGACY_MAIL_DECOY_RECEIPT_OPERATION_ID);

    let now = ctx.timestamp.to_micros_since_unix_epoch();
    let receipt_template = AuctionOperationReceipt {
        operation_id: 0,
        auction_id: 0,
        actor_guid: 0,
        item_guid: 0,
        item_entry: 0,
        item_stack_count: 1,
        item_durability: 0,
        item_enchant_id: 0,
        item_soulbound: false,
        start_bid: 100,
        buyout: 500,
        duration_minutes: 480,
        deposit: 10,
        created_micros: now,
        expires_micros: now + 1,
        house: 0,
        deposit_rate: 5,
        consignment_rate: 5,
        random_property_id: 0,
        item_text_id: 0,
    };
    receipts.insert(AuctionOperationReceipt {
        operation_id: LEGACY_MAIL_WON_RECEIPT_OPERATION_ID,
        auction_id: LEGACY_MAIL_WON_RECEIPT_AUCTION_ID,
        actor_guid: LEGACY_MAIL_FIXTURE_RECIPIENT_GUID + 1,
        item_entry: LEGACY_MAIL_FIXTURE_ITEM_ENTRY,
        house: LEGACY_MAIL_WON_RECEIPT_HOUSE,
        ..receipt_template
    });
    receipts.insert(AuctionOperationReceipt {
        operation_id: LEGACY_MAIL_SOLD_RECEIPT_OPERATION_ID,
        auction_id: LEGACY_MAIL_SOLD_RECEIPT_AUCTION_ID,
        actor_guid: LEGACY_MAIL_SOLD_FIXTURE_RECIPIENT_GUID,
        item_entry: LEGACY_MAIL_SOLD_RECEIPT_ITEM_ENTRY,
        random_property_id: 117,
        house: LEGACY_MAIL_SOLD_RECEIPT_HOUSE,
        ..receipt_template
    });
    // A refused listing: the `auction_id == 0` sentinel, excluded outright regardless of price.
    receipts.insert(AuctionOperationReceipt {
        operation_id: LEGACY_MAIL_REFUSED_RECEIPT_OPERATION_ID,
        auction_id: 0,
        actor_guid: LEGACY_MAIL_SOLD_FIXTURE_RECIPIENT_GUID,
        item_entry: LEGACY_MAIL_REFUSED_RECEIPT_ITEM_ENTRY,
        house: LEGACY_MAIL_WRONG_RECEIPT_HOUSE,
        ..receipt_template
    });
    // A second real listing: seller_proceeds(50, 10, 5) == 58 through seller_proceeds(200, 10, 5)
    // == 200, a range that never reaches the Sold mail's money (485).
    receipts.insert(AuctionOperationReceipt {
        operation_id: LEGACY_MAIL_DECOY_RECEIPT_OPERATION_ID,
        auction_id: LEGACY_MAIL_DECOY_RECEIPT_AUCTION_ID,
        actor_guid: LEGACY_MAIL_SOLD_FIXTURE_RECIPIENT_GUID,
        item_entry: LEGACY_MAIL_DECOY_RECEIPT_ITEM_ENTRY,
        start_bid: 50,
        buyout: 200,
        house: LEGACY_MAIL_WRONG_RECEIPT_HOUSE,
        ..receipt_template
    });

    // Every row enters `game_mail` through `mail::insert_letter` (the sole writer), so every
    // fixture goes through `Letter::from_character` exactly as the sending path that produced each
    // shape did.
    crate::mail::insert_letter(
        ctx,
        crate::mail::Letter::from_character(
            LEGACY_MAIL_FIXTURE_RECIPIENT_GUID + 1,
            LEGACY_MAIL_FIXTURE_RECIPIENT_GUID,
            "Auction won".to_string(),
            String::new(),
            0,
            0,
            crate::items::ItemSnapshot {
                entry: LEGACY_MAIL_FIXTURE_ITEM_ENTRY,
                stack_count: 1,
                durability: 0,
                enchant_id: 0,
                soulbound: false,
                random_property_id: 0,
                item_text_id: 0,
            },
        ),
    );
    crate::mail::insert_letter(
        ctx,
        crate::mail::Letter::from_character(
            LEGACY_MAIL_LOOKALIKE_SENDER_GUID,
            LEGACY_MAIL_LOOKALIKE_RECIPIENT_GUID,
            "Auction won".to_string(),
            String::new(),
            0,
            LEGACY_MAIL_LOOKALIKE_COD,
            crate::items::ItemSnapshot {
                entry: LEGACY_MAIL_LOOKALIKE_ITEM_ENTRY,
                stack_count: 1,
                durability: 0,
                enchant_id: 0,
                soulbound: false,
                random_property_id: 0,
                item_text_id: 0,
            },
        ),
    );
    crate::mail::insert_letter(
        ctx,
        crate::mail::Letter::from_character(
            LEGACY_MAIL_NO_COD_LOOKALIKE_SENDER_GUID,
            LEGACY_MAIL_NO_COD_LOOKALIKE_RECIPIENT_GUID,
            "Auction won".to_string(),
            String::new(),
            0,
            0,
            crate::items::ItemSnapshot {
                entry: LEGACY_MAIL_NO_COD_LOOKALIKE_ITEM_ENTRY,
                stack_count: 1,
                durability: 0,
                enchant_id: 0,
                soulbound: false,
                random_property_id: 0,
                item_text_id: 0,
            },
        ),
    );
    crate::mail::insert_letter(
        ctx,
        crate::mail::Letter::from_character(
            0,
            LEGACY_MAIL_SOLD_FIXTURE_RECIPIENT_GUID,
            "Auction sold".to_string(),
            String::new(),
            485,
            0,
            crate::items::ItemSnapshot::default(),
        ),
    );
    Ok(())
}

/// Verify `debug_repair_after_publish` re-tagged both legitimate legacy rows to the vanilla
/// `AuctionHouse` sender, each receipt's own house, a machine subject built from the item that
/// receipt names, and COPIED — and left both look-alikes exactly as sent: still `Character` mail,
/// one still carrying its cash on delivery price, the other still from its uninvolved real sender.
/// Run twice in a row in the durable test to prove the repair is idempotent.
#[reducer]
pub fn debug_verify_legacy_auction_mail_repaired(ctx: &ReducerContext) -> Result<(), String> {
    crate::helpers::require_operator(ctx)?;
    let mut matches: Vec<crate::Mail> = ctx
        .db
        .game_mail()
        .by_recipient()
        .filter(&LEGACY_MAIL_FIXTURE_RECIPIENT_GUID)
        .collect();
    if matches.len() != 1 {
        return Err(format!(
            "expected exactly one legacy auction mail fixture row, found {}",
            matches.len()
        ));
    }
    let mail = matches.remove(0);
    let expected_subject =
        auction_mail_subject(LEGACY_MAIL_FIXTURE_ITEM_ENTRY, 0, AuctionMailAction::Won);
    if mail.sender() != MailSender::AuctionHouse(LEGACY_MAIL_WON_RECEIPT_HOUSE)
        || mail.subject != expected_subject
        || mail.check_flags != CHECK_MASK_COPIED
    {
        return Err(format!(
            "legacy Won mail was not repaired: sender={:?}, subject={:?}, check_flags={}",
            mail.sender(),
            mail.subject,
            mail.check_flags
        ));
    }

    let mut sold_matches: Vec<crate::Mail> = ctx
        .db
        .game_mail()
        .by_recipient()
        .filter(&LEGACY_MAIL_SOLD_FIXTURE_RECIPIENT_GUID)
        .collect();
    if sold_matches.len() != 1 {
        return Err(format!(
            "expected exactly one legacy auction mail fixture row, found {}",
            sold_matches.len()
        ));
    }
    let sold = sold_matches.remove(0);
    let expected_sold_subject = auction_mail_subject(
        LEGACY_MAIL_SOLD_RECEIPT_ITEM_ENTRY,
        117,
        AuctionMailAction::Successful,
    );
    if sold.sender() != MailSender::AuctionHouse(LEGACY_MAIL_SOLD_RECEIPT_HOUSE)
        || sold.subject != expected_sold_subject
        || sold.check_flags != CHECK_MASK_COPIED
    {
        return Err(format!(
            "legacy Sold mail was not repaired: sender={:?}, subject={:?}, check_flags={}",
            sold.sender(),
            sold.subject,
            sold.check_flags
        ));
    }

    let mut lookalike_matches: Vec<crate::Mail> = ctx
        .db
        .game_mail()
        .by_recipient()
        .filter(&LEGACY_MAIL_LOOKALIKE_RECIPIENT_GUID)
        .collect();
    if lookalike_matches.len() != 1 {
        return Err(format!(
            "expected exactly one look-alike player mail row, found {}",
            lookalike_matches.len()
        ));
    }
    let lookalike = lookalike_matches.remove(0);
    if lookalike.sender() != MailSender::Character(LEGACY_MAIL_LOOKALIKE_SENDER_GUID)
        || lookalike.subject != "Auction won"
        || lookalike.cod != LEGACY_MAIL_LOOKALIKE_COD
    {
        return Err(format!(
            "the repair touched a real player's look-alike mail: sender={:?}, subject={:?}, cod={}",
            lookalike.sender(),
            lookalike.subject,
            lookalike.cod
        ));
    }

    let mut no_cod_lookalike_matches: Vec<crate::Mail> = ctx
        .db
        .game_mail()
        .by_recipient()
        .filter(&LEGACY_MAIL_NO_COD_LOOKALIKE_RECIPIENT_GUID)
        .collect();
    if no_cod_lookalike_matches.len() != 1 {
        return Err(format!(
            "expected exactly one no-COD look-alike player mail row, found {}",
            no_cod_lookalike_matches.len()
        ));
    }
    let no_cod_lookalike = no_cod_lookalike_matches.remove(0);
    if no_cod_lookalike.sender() != MailSender::Character(LEGACY_MAIL_NO_COD_LOOKALIKE_SENDER_GUID)
        || no_cod_lookalike.subject != "Auction won"
        || no_cod_lookalike.cod != 0
    {
        return Err(format!(
            "the repair touched a real player's no-COD look-alike mail: sender={:?}, subject={:?}",
            no_cod_lookalike.sender(),
            no_cod_lookalike.subject,
        ));
    }
    Ok(())
}
