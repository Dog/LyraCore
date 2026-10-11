//! Table Relays for this Protocol Family.

use crate::codec::{self};
use crate::stdb::bindings::*;
use crate::stdb::world_view::*;
use crate::world::Outbound;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;
use wow_world_messages::vanilla::AuctionHouse;

/// Auction Notice: the packet body both legs run. Audience resolved by the caller. Outbid and Won
/// go to the bidder on `SMSG_AUCTION_BIDDER_NOTIFICATION`; Sold, Expired and New bid go to the
/// owner on `SMSG_AUCTION_OWNER_NOTIFICATION`; Removed goes to the bidder a Cancellation displaced
/// on `SMSG_AUCTION_REMOVED_NOTIFICATION` (`cm:AuctionHouseHandler.cpp`/`AuctionHouseMgr.cpp`).
/// `house` outside the imported 1-7 range and any other `kind` both drop the notice and log —
/// the accompanying Auction Mail still reaches the recipient's inbox either way.
pub(crate) fn auction_notice_outbound(row: &AuctionNotice) -> Vec<Outbound> {
    use lyracore_shared::auction::auction_notice::{EXPIRED, NEW_BID, OUTBID, REMOVED, SOLD, WON};

    let Ok(house) = AuctionHouse::try_from(row.house) else {
        log::warn!(
            "auction notice: house {} is not an imported auction house; dropping kind {}",
            row.house,
            row.kind
        );
        return Vec::new();
    };
    match row.kind {
        OUTBID | WON => vec![Outbound::One(
            ServerOpcodeMessage::SMSG_AUCTION_BIDDER_NOTIFICATION(Box::new(
                codec::build_auction_bidder_notification(
                    house,
                    row.auction_id,
                    row.bidder_guid,
                    row.bid,
                    row.out_bid,
                    row.item_entry,
                    row.random_property_id,
                ),
            )),
        )],
        SOLD | EXPIRED | NEW_BID => vec![Outbound::One(
            ServerOpcodeMessage::SMSG_AUCTION_OWNER_NOTIFICATION(Box::new(
                codec::build_auction_owner_notification(
                    row.auction_id,
                    row.bid,
                    row.out_bid,
                    row.bidder_guid,
                    row.item_entry,
                    row.random_property_id,
                ),
            )),
        )],
        REMOVED => vec![Outbound::One(
            ServerOpcodeMessage::SMSG_AUCTION_REMOVED_NOTIFICATION(Box::new(
                codec::build_auction_removed_notification(
                    row.auction_id,
                    row.item_entry,
                    row.random_property_id,
                ),
            )),
        )],
        other => {
            log::warn!("auction notice: unknown kind {other}; dropping");
            Vec::new()
        }
    }
}

/// An Auction Notice landed → SMSG_AUCTION_BIDDER_NOTIFICATION or SMSG_AUCTION_OWNER_NOTIFICATION
/// to the row's RECIPIENT and nobody else (same shape as [`group_event_appeared`]). An offline
/// recipient gets the Auction Mail the same transaction wrote and no packet — the vanilla rule.
pub(crate) fn auction_notice_appeared(view: &WorldView, row: &AuctionNotice) {
    let Some(session) = view.session_of_owner(row.recipient_guid) else {
        return;
    };
    let Some(viewer) = view.viewer(session) else {
        return;
    };
    if !crate::codec::social::subscriptions::private_recipient_audience(
        row.recipient_guid,
        viewer.self_guid,
    ) {
        return;
    }
    let row = row.clone();
    enqueue(viewer.clone(), move |_| {
        crate::codec::auction::subscriptions::auction_notice_outbound(&row)
    });
}

crate::stdb::relays::declare_relays! {
    game_auction_notice [true] {
        insert(Live) => |context, _event, row| auction_notice_appeared(&context.view, row);
    }
}
