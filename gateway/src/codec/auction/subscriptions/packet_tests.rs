use super::*;
use crate::stdb::bindings::AuctionNotice;
use crate::world::Outbound;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

#[test]
fn auction_notice_kinds_decode_to_their_own_packet_and_an_unknown_one_drops() {
    use lyracore_shared::auction::auction_notice::{EXPIRED, NEW_BID, OUTBID, REMOVED, SOLD, WON};
    use wow_world_messages::vanilla::AuctionHouse;

    let notice = |kind: u8, house: u32| AuctionNotice {
        id: 1,
        recipient_guid: 7,
        kind,
        house,
        auction_id: 41,
        item_entry: 25,
        random_property_id: 117,
        bid: 500,
        out_bid: 25,
        bidder_guid: 9,
        created_at: spacetimedb_sdk::Timestamp::UNIX_EPOCH,
    };

    for kind in [OUTBID, WON] {
        let out = auction_notice_outbound(&notice(kind, 1));
        assert_eq!(
            out.len(),
            1,
            "kind {kind} must decode to exactly one packet"
        );
        match &out[0] {
            Outbound::One(ServerOpcodeMessage::SMSG_AUCTION_BIDDER_NOTIFICATION(packet)) => {
                assert_eq!(packet.auction_house, AuctionHouse::Stormwind);
                assert_eq!(packet.bidder.guid(), 9);
            }
            Outbound::One(other) => {
                panic!("kind {kind}: expected SMSG_AUCTION_BIDDER_NOTIFICATION, got {other}")
            }
            _ => {
                panic!("kind {kind}: expected a single SMSG_AUCTION_BIDDER_NOTIFICATION packet")
            }
        }
    }

    for kind in [SOLD, EXPIRED, NEW_BID] {
        let out = auction_notice_outbound(&notice(kind, 1));
        assert_eq!(
            out.len(),
            1,
            "kind {kind} must decode to exactly one packet"
        );
        match &out[0] {
            Outbound::One(ServerOpcodeMessage::SMSG_AUCTION_OWNER_NOTIFICATION(packet)) => {
                assert_eq!(packet.auction_id, 41);
            }
            Outbound::One(other) => {
                panic!("kind {kind}: expected SMSG_AUCTION_OWNER_NOTIFICATION, got {other}")
            }
            _ => {
                panic!("kind {kind}: expected a single SMSG_AUCTION_OWNER_NOTIFICATION packet")
            }
        }
    }

    let out = auction_notice_outbound(&notice(REMOVED, 1));
    assert!(
        matches!(
            out.as_slice(),
            [Outbound::One(ServerOpcodeMessage::SMSG_AUCTION_REMOVED_NOTIFICATION(packet))]
                if packet.item == 41
                    && packet.item_template == 25
                    && packet.random_property_id == 117
        ),
        "Removed goes out on SMSG_AUCTION_REMOVED_NOTIFICATION"
    );

    assert!(
        auction_notice_outbound(&notice(200, 1)).is_empty(),
        "an unknown kind must drop, not guess a packet"
    );
    assert!(
        auction_notice_outbound(&notice(OUTBID, 99)).is_empty(),
        "a house outside 1-7 must drop rather than send an unresolved house"
    );
}
