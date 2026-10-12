use super::*;

fn policy() -> AuctionHousePolicy {
    AuctionHousePolicy {
        id: 1,
        deposit_rate: 5,
        consignment_rate: 5,
    }
}

#[test]
fn seller_proceeds_apply_the_imported_cut_and_return_the_deposit_checked() {
    assert_eq!(seller_proceeds(100, 10, 5), Some(105));
    assert_eq!(seller_proceeds(100, 10, 15), Some(95));
    assert_eq!(seller_proceeds(19, 1, 5), Some(20));
    assert_eq!(seller_proceeds(20, 1, 5), Some(20));
    assert_eq!(seller_proceeds(u32::MAX, u32::MAX, 5), None);
    assert_eq!(seller_proceeds(100, 10, 101), None);
}

// The subject is "{item_entry}:{random_property_id}:{action}" (`cm:AuctionHouseMgr.cpp:134,
// 181,229`), and the action codes are `MailAuctionAnswers` (`cm:Mail.h:100-109`): every string
// below is written out by hand from that format, not produced by calling the function under
// test with different inputs.
#[test]
fn the_subject_is_the_item_the_random_property_and_the_mail_auction_answers_code() {
    assert_eq!(
        auction_mail_subject(1234, 56, AuctionMailAction::Outbid),
        "1234:56:0"
    );
    assert_eq!(
        auction_mail_subject(1234, 56, AuctionMailAction::Won),
        "1234:56:1"
    );
    assert_eq!(
        auction_mail_subject(1234, 56, AuctionMailAction::Successful),
        "1234:56:2"
    );
    assert_eq!(
        auction_mail_subject(1234, 56, AuctionMailAction::Expired),
        "1234:56:3"
    );
    assert_eq!(
        auction_mail_subject(1234, 56, AuctionMailAction::CancelledToBidder),
        "1234:56:4"
    );
    assert_eq!(
        auction_mail_subject(1234, 0, AuctionMailAction::Cancelled),
        "1234:0:5",
        "a plain item's random property id is 0"
    );
}

fn mail_for_body(action: AuctionMailAction) -> AuctionMail {
    AuctionMail {
        recipient_guid: 1,
        house: 1,
        action,
        item_entry: 1234,
        random_property_id: 56,
        money: 0,
        attached_item: crate::items::ItemSnapshot::default(),
        counterparty_guid: 0,
        bid: 0,
        buyout: 0,
        deposit: 0,
        cut: 0,
    }
}

/// The invoice body's guid is lowercase hex, right-aligned in a 16-character, space-filled
/// field (`fx:MailFrame.lua:300-361`): the padding widths below are counted by hand, not
/// produced by the `{:>16x}` specifier under test.
#[test]
fn the_won_and_successful_bodies_are_the_padded_invoice_cmangos_sends() {
    let won = AuctionMail {
        counterparty_guid: 9,
        bid: 100,
        buyout: 500,
        ..mail_for_body(AuctionMailAction::Won)
    };
    assert_eq!(
        auction_mail_body(&won),
        format!("{}9:100:500", " ".repeat(15)),
        "guid 9 is one hex digit, padded with 15 leading spaces to a 16-wide field"
    );

    let successful = AuctionMail {
        counterparty_guid: 0x123,
        bid: 250,
        buyout: 0,
        deposit: 12,
        cut: 6,
        ..mail_for_body(AuctionMailAction::Successful)
    };
    assert_eq!(
        auction_mail_body(&successful),
        format!("{}123:250:0:12:6", " ".repeat(13)),
        "guid 0x123 is three hex digits, padded with 13 leading spaces"
    );
}

#[test]
fn outbid_expired_and_cancelled_mail_carries_no_body() {
    for action in [
        AuctionMailAction::Outbid,
        AuctionMailAction::Expired,
        AuctionMailAction::CancelledToBidder,
        AuctionMailAction::Cancelled,
    ] {
        assert_eq!(auction_mail_body(&mail_for_body(action)), "");
    }
}

#[test]
fn outbid_notice_fires_only_for_a_real_displaced_bidder() {
    let item = crate::items::ItemSnapshot {
        entry: 1234,
        random_property_id: 56,
        ..crate::items::ItemSnapshot::default()
    };
    assert_eq!(
        outbid_notice(1, 41, item, 0, 8, 0),
        None,
        "a fresh listing's first bid displaces nobody"
    );
    assert_eq!(
        outbid_notice(1, 41, item, 9, 8, 100),
        Some(AuctionNoticeDraft {
            recipient_guid: 9,
            kind: auction_notice::OUTBID,
            house: 1,
            auction_id: 41,
            item_entry: 1234,
            random_property_id: 56,
            bid: 100,
            out_bid: 5, // vanilla minimum raise on a 100 bid: 5% rounded up
            bidder_guid: 9,
        })
    );
}

/// cmangos's `UpdateBid` never calls `SendAuctionBidderNotification` when the new bidder is
/// the bidder it displaces — raising your own bid is not being outbid
/// (`cm:AuctionHouseMgr.cpp:780-793`).
#[test]
fn a_bidder_who_raises_their_own_bid_gets_no_outbid_notice() {
    let item = crate::items::ItemSnapshot {
        entry: 1234,
        random_property_id: 56,
        ..crate::items::ItemSnapshot::default()
    };
    assert_eq!(
        outbid_notice(1, 41, item, 9, 9, 100),
        None,
        "the displaced bidder and the new bidder are the same Character"
    );
}

#[test]
fn new_bid_and_expired_notices_address_the_owner() {
    let item = crate::items::ItemSnapshot {
        entry: 1234,
        random_property_id: 56,
        ..crate::items::ItemSnapshot::default()
    };
    assert_eq!(
        new_bid_notice(1, 41, item, 7, 9, 107),
        AuctionNoticeDraft {
            recipient_guid: 7,
            kind: auction_notice::NEW_BID,
            house: 1,
            auction_id: 41,
            item_entry: 1234,
            random_property_id: 56,
            bid: 107,
            out_bid: 6, // vanilla minimum raise on a 107 bid: 5% rounded up
            bidder_guid: 9,
        }
    );
    assert_eq!(
        expired_notice(1, 41, item, 7),
        AuctionNoticeDraft {
            recipient_guid: 7,
            kind: auction_notice::EXPIRED,
            house: 1,
            auction_id: 41,
            item_entry: 1234,
            random_property_id: 56,
            bid: 0,
            // bid_increment(0) is 0, but expired_notice floors an unsold listing's raise to 1.
            out_bid: 1,
            bidder_guid: 0,
        }
    );
}

#[test]
fn settlement_notices_are_won_to_the_buyer_and_sold_to_the_seller() {
    let item = crate::items::ItemSnapshot {
        entry: 1234,
        random_property_id: 56,
        ..crate::items::ItemSnapshot::default()
    };
    let [won, sold] = settlement_notices(1, 41, item.entry, item.random_property_id, 7, 9, 500);
    assert_eq!(
        won,
        AuctionNoticeDraft {
            recipient_guid: 9,
            kind: auction_notice::WON,
            house: 1,
            auction_id: 41,
            item_entry: 1234,
            random_property_id: 56,
            bid: 0,
            out_bid: 25, // vanilla minimum raise on a 500 bid: 5% rounded up
            bidder_guid: 9,
        }
    );
    assert_eq!(
        sold,
        AuctionNoticeDraft {
            recipient_guid: 7,
            kind: auction_notice::SOLD,
            house: 1,
            auction_id: 41,
            item_entry: 1234,
            random_property_id: 56,
            bid: 500,
            out_bid: 25, // vanilla minimum raise on a 500 bid: 5% rounded up
            bidder_guid: 0,
        }
    );
}

#[test]
fn every_legacy_auction_subject_maps_to_its_action_and_an_unknown_one_does_not() {
    assert_eq!(
        legacy_auction_action("Auction outbid"),
        Some(AuctionMailAction::Outbid)
    );
    assert_eq!(
        legacy_auction_action("Auction won"),
        Some(AuctionMailAction::Won)
    );
    assert_eq!(
        legacy_auction_action("Auction sold"),
        Some(AuctionMailAction::Successful)
    );
    assert_eq!(
        legacy_auction_action("Auction expired"),
        Some(AuctionMailAction::Expired)
    );
    assert_eq!(
        legacy_auction_action("Auction listing refused"),
        Some(AuctionMailAction::Cancelled)
    );
    assert_eq!(
        legacy_auction_action("Auction bid refund"),
        Some(AuctionMailAction::Outbid),
        "the deferred bid refund has no vanilla twin and renders as Outbid"
    );
    assert_eq!(legacy_auction_action("Auction House of Cards"), None);
    assert_eq!(legacy_auction_action("meet me at the gate"), None);
}

#[test]
fn listing_deposit_uses_the_imported_rate_and_supported_duration_ladder() {
    // Hand-derived from cm:AuctionHouseMgr.cpp:65-77's
    // `SellPrice * count * (etime / MIN_AUCTION_TIME) * depositPercent / 100`.
    assert_eq!(listing_deposit(100, 2, 120, 5), Some(10));
    assert_eq!(listing_deposit(100, 2, 480, 5), Some(40));
    assert_eq!(listing_deposit(100, 2, 1_440, 5), Some(120));
    assert_eq!(listing_deposit(100, 2, 120, 25), Some(50));
    assert_eq!(listing_deposit(1, 1, 120, 5), Some(1));
    assert_eq!(
        listing_deposit(100, 1, 60, 5),
        None,
        "no client offers a 1-hour listing"
    );
    assert_eq!(
        listing_deposit(100, 1, 720, 5),
        None,
        "720 is the TBC 12-hour duration; no 1.12 client sends it"
    );
    assert_eq!(
        listing_deposit(100, 1, 2_880, 5),
        None,
        "2_880 is the TBC 48-hour duration; no 1.12 client sends it"
    );
    assert_eq!(listing_deposit(u32::MAX, u32::MAX, 1_440, 5), None);
    assert_eq!(listing_deposit(100, 1, 120, 101), None);
}

fn item(slot: u8) -> ListingItem {
    ListingItem {
        guid: 70,
        owner_guid: 7,
        slot,
        mailable: true,
        snapshot: crate::items::ItemSnapshot {
            entry: 25,
            stack_count: 2,
            durability: 17,
            enchant_id: 9,
            soulbound: false,
            random_property_id: 117,
            item_text_id: 0,
        },
        sell_price: 100,
    }
}

fn active_bid_auction() -> BidAuction {
    let snapshot = item(23).snapshot;
    BidAuction {
        id: 41,
        house: 1,
        owner_guid: 7,
        item: snapshot,
        highest_bidder_guid: 0,
        highest_bid: 0,
        start_bid: 100,
        buyout: 0,
        deposit: 10,
        consignment_rate: 5,
        expires_micros: 2_000,
        revision: 3,
    }
}

fn accepted_active(
    price: u32,
    revision: u64,
    displaced_bidder_guid: u64,
    displaced_bid: u32,
) -> HoldDecision {
    HoldDecision::Accepted(BidAcceptance {
        price,
        effect: AuctionBidEffect::RemainActive { revision },
        displaced_bidder_guid,
        displaced_bid,
    })
}

fn accepted_buyout(price: u32, displaced_bidder_guid: u64, displaced_bid: u32) -> HoldDecision {
    HoldDecision::Accepted(BidAcceptance {
        price,
        effect: AuctionBidEffect::SettleBuyout,
        displaced_bidder_guid,
        displaced_bid,
    })
}

fn terms() -> ListingTerms {
    ListingTerms {
        start_bid: 10,
        buyout: 20,
        duration_minutes: 120,
    }
}

#[test]
fn listing_accepts_only_one_owned_mailable_bag_stack_and_valid_terms() {
    assert_eq!(
        prepare_listing(Some(&item(23)), 7, 10, terms(), policy()),
        Ok(10)
    );
    assert_eq!(
        prepare_listing(Some(&item(120)), 7, 10, terms(), policy()),
        Ok(10)
    );

    for slot in [15, 19, 39, 63, 119, 192] {
        assert_eq!(
            prepare_listing(Some(&item(slot)), 7, 10, terms(), policy()),
            Err(AuctionRefusal::ItemNotFound),
            "slot {slot} must not be auctionable"
        );
    }

    let mut foreign = item(23);
    foreign.owner_guid = 8;
    assert_eq!(
        prepare_listing(Some(&foreign), 7, 10, terms(), policy()),
        Err(AuctionRefusal::ItemNotFound)
    );

    let mut soulbound = item(23);
    soulbound.snapshot.soulbound = true;
    assert_eq!(
        prepare_listing(Some(&soulbound), 7, 10, terms(), policy()),
        Err(AuctionRefusal::ItemNotFound)
    );

    let mut not_mailable = item(120);
    not_mailable.mailable = false;
    assert_eq!(
        prepare_listing(Some(&not_mailable), 7, 10, terms(), policy()),
        Err(AuctionRefusal::ItemNotFound)
    );

    let mut letter = item(23);
    letter.snapshot.item_text_id = 8;
    assert_eq!(
        prepare_listing(Some(&letter), 7, 10, terms(), policy()),
        Ok(10),
        "a Letter Copy's Plain Letter lists, and its text id travels in the snapshot"
    );

    assert_eq!(
        prepare_listing(None, 7, 10, terms(), policy()),
        Err(AuctionRefusal::ItemNotFound)
    );
    assert_eq!(
        prepare_listing(Some(&item(23)), 7, 9, terms(), policy()),
        Err(AuctionRefusal::NotEnoughMoney)
    );

    for invalid in [
        ListingTerms {
            start_bid: 0,
            ..terms()
        },
        ListingTerms {
            start_bid: 20,
            buyout: 19,
            ..terms()
        },
        ListingTerms {
            duration_minutes: 60,
            ..terms()
        },
        ListingTerms {
            duration_minutes: 720,
            ..terms()
        },
        ListingTerms {
            duration_minutes: 2_880,
            ..terms()
        },
    ] {
        assert_eq!(
            prepare_listing(Some(&item(23)), 7, 10, invalid, policy()),
            Err(AuctionRefusal::InvalidTerms),
            "{invalid:?} must not list: the 1.12 client never sends this duration"
        );
    }
}

#[derive(Clone)]
struct FakeLocal {
    money: Option<u32>,
    item: Option<ListingItem>,
    now_micros: i64,
    next_auction_id: u32,
    committed: Option<(PreparedListing, u32)>,
}

impl ListingSource for FakeLocal {
    fn seller_money(&self, _seller_guid: u64) -> Option<u32> {
        self.money
    }

    fn item(&self, _item_guid: u64) -> Option<ListingItem> {
        self.item.clone()
    }

    fn now_micros(&self) -> i64 {
        self.now_micros
    }
}

impl LocalListingSink for FakeLocal {
    fn receipt(&self, operation_id: u64) -> Option<ListingReceipt> {
        self.committed
            .as_ref()
            .filter(|(listing, _)| listing.request.operation_id == operation_id)
            .map(|(listing, auction_id)| ListingReceipt {
                listing: listing.clone(),
                auction_id: *auction_id,
            })
    }

    fn commit_local(&mut self, listing: PreparedListing) -> u32 {
        self.money = self.money.map(|money| money - listing.deposit);
        self.item = None;
        let auction_id = self.next_auction_id;
        self.next_auction_id += 1;
        self.committed = Some((listing, auction_id));
        auction_id
    }
}

fn request() -> ListingRequest {
    ListingRequest {
        operation_id: 900,
        seller_guid: 7,
        item_guid: 70,
        house: policy(),
        terms: terms(),
    }
}

fn local() -> FakeLocal {
    FakeLocal {
        money: Some(50),
        item: Some(item(23)),
        now_micros: 1_000,
        next_auction_id: 41,
        committed: None,
    }
}

#[test]
fn local_listing_atomically_moves_the_exact_item_and_deposit_into_one_scheduled_auction() {
    let mut store = local();

    assert_eq!(create_local_listing(&mut store, request()), Ok(41));

    assert_eq!(store.money, Some(40));
    assert!(store.item.is_none());
    let (listing, auction_id) = store.committed.as_ref().expect("one committed Auction");
    assert_eq!(*auction_id, 41);
    assert_eq!(listing.snapshot, item(23).snapshot);
    assert_eq!(listing.deposit, 10);
    assert_eq!(listing.created_micros, 1_000);
    assert_eq!(listing.expires_micros, 7_200_001_000);

    assert_eq!(create_local_listing(&mut store, request()), Ok(41));
    assert_eq!(store.money, Some(40), "replay must not charge again");
    assert_eq!(store.next_auction_id, 42, "replay must not mint another id");
}

#[test]
fn local_listing_refusals_leave_item_purse_and_market_unchanged() {
    let cases = [
        {
            let mut store = local();
            store.money = Some(9);
            (store, request())
        },
        {
            let mut bad = request();
            bad.terms.duration_minutes = 60;
            (local(), bad)
        },
        {
            let mut bad = request();
            bad.operation_id = 0;
            (local(), bad)
        },
    ];

    for (mut store, request) in cases {
        let before = store.clone();
        assert!(create_local_listing(&mut store, request).is_err());
        assert_eq!(store.money, before.money);
        assert_eq!(store.item, before.item);
        assert_eq!(store.next_auction_id, before.next_auction_id);
        assert!(store.committed.is_none());
    }
}

#[derive(Clone)]
struct FakeSource {
    money: Option<u32>,
    item: Option<ListingItem>,
    now_micros: i64,
    hold: Option<ListingHold>,
    receipt: Option<ListingReceipt>,
}

impl ListingSource for FakeSource {
    fn seller_money(&self, _seller_guid: u64) -> Option<u32> {
        self.money
    }

    fn item(&self, _item_guid: u64) -> Option<ListingItem> {
        self.item.clone()
    }

    fn now_micros(&self) -> i64 {
        self.now_micros
    }
}

impl HoldSink for FakeSource {
    fn hold(&self, operation_id: u64) -> Option<ListingHold> {
        self.hold
            .as_ref()
            .filter(|hold| hold.listing.request.operation_id == operation_id)
            .cloned()
    }

    fn receipt(&self, operation_id: u64) -> Option<ListingReceipt> {
        self.receipt
            .as_ref()
            .filter(|receipt| receipt.listing.request.operation_id == operation_id)
            .cloned()
    }

    fn commit_hold(&mut self, hold: ListingHold) {
        self.money = self.money.map(|money| money - hold.listing.deposit);
        self.item = None;
        self.hold = Some(hold);
    }

    fn confirm_hold(&mut self, receipt: ListingReceipt) {
        self.receipt = Some(receipt);
    }

    fn delete_hold(&mut self, operation_id: u64) {
        if self
            .hold
            .as_ref()
            .is_some_and(|hold| hold.listing.request.operation_id == operation_id)
        {
            self.hold = None;
        }
    }
}

#[derive(Clone, Default)]
struct FakeRefundCore {
    refund: Option<ListingRefund>,
    mails: Vec<AuctionMail>,
}

impl ListingRefundSink for FakeRefundCore {
    fn refund(&self, operation_id: u64) -> Result<Option<ListingRefund>, AuctionRefusal> {
        Ok(self
            .refund
            .as_ref()
            .filter(|refund| refund.listing.request.operation_id == operation_id)
            .cloned())
    }

    fn commit_refund(&mut self, refund: ListingRefund) {
        self.mails.push(refund.mail);
        self.refund = Some(refund);
    }
}

#[derive(Clone)]
struct FakeMarket {
    next_auction_id: u32,
    receipt: Option<ListingReceipt>,
    auction_count: usize,
}

impl MarketSink for FakeMarket {
    fn receipt(&self, operation_id: u64) -> Option<ListingReceipt> {
        self.receipt
            .as_ref()
            .filter(|receipt| receipt.listing.request.operation_id == operation_id)
            .cloned()
    }

    fn commit_market(&mut self, listing: PreparedListing) -> u32 {
        let auction_id = self.next_auction_id;
        self.next_auction_id += 1;
        self.auction_count += 1;
        self.receipt = Some(ListingReceipt {
            listing,
            auction_id,
        });
        auction_id
    }
}

fn source() -> FakeSource {
    FakeSource {
        money: Some(50),
        item: Some(item(23)),
        now_micros: 1_000,
        hold: None,
        receipt: None,
    }
}

#[test]
fn listing_rejects_overflowing_possible_proceeds_before_moving_value() {
    for terms in [
        ListingTerms {
            start_bid: u32::MAX,
            buyout: 0,
            duration_minutes: 120,
        },
        ListingTerms {
            start_bid: 10,
            buyout: u32::MAX,
            duration_minutes: 120,
        },
    ] {
        let mut local = local();
        local.money = Some(u32::MAX);
        local.item.as_mut().unwrap().sell_price = u32::MAX;
        let before = local.clone();
        let request = ListingRequest { terms, ..request() };

        assert_eq!(
            create_local_listing(&mut local, request),
            Err(AuctionRefusal::InvalidTerms)
        );
        assert_eq!(local.money, before.money);
        assert_eq!(local.item, before.item);
        assert!(local.committed.is_none());

        let mut source = source();
        source.money = Some(u32::MAX);
        source.item.as_mut().unwrap().sell_price = u32::MAX;
        let before = source.clone();

        assert_eq!(
            fence_listing(&mut source, request),
            Err(AuctionRefusal::InvalidTerms)
        );
        assert_eq!(source.money, before.money);
        assert_eq!(source.item, before.item);
        assert!(source.hold.is_none());
        assert!(source.receipt.is_none());
    }
}

fn market() -> FakeMarket {
    FakeMarket {
        next_auction_id: 41,
        receipt: None,
        auction_count: 0,
    }
}

fn drive_sharded(
    source: &mut FakeSource,
    market: &mut FakeMarket,
    request: ListingRequest,
) -> Result<u32, AuctionRefusal> {
    fence_listing(source, request)?;
    if let Some(receipt) = source.receipt(request.operation_id) {
        settle_listing(source, request.operation_id)?;
        return Ok(receipt.auction_id);
    }
    let hold = source
        .hold(request.operation_id)
        .expect("a successful fresh fence leaves a durable Hold");
    let auction_id = commit_held_listing(market, hold.listing.clone())?;
    confirm_listing(
        source,
        ListingReceipt {
            listing: hold.listing,
            auction_id,
        },
    )?;
    settle_listing(source, request.operation_id)?;
    Ok(auction_id)
}

#[test]
fn sharded_listing_retries_every_interruption_without_losing_or_duplicating_value() {
    for killed_after in 0..=3 {
        let mut source = source();
        let mut market = market();
        let request = request();

        fence_listing(&mut source, request).expect("fenced");
        if killed_after >= 1 {
            let hold = source.hold(request.operation_id).unwrap();
            let auction_id = commit_held_listing(&mut market, hold.listing.clone()).unwrap();
            if killed_after >= 2 {
                confirm_listing(
                    &mut source,
                    ListingReceipt {
                        listing: hold.listing,
                        auction_id,
                    },
                )
                .unwrap();
                if killed_after >= 3 {
                    settle_listing(&mut source, request.operation_id).unwrap();
                }
            }
        }

        assert_eq!(drive_sharded(&mut source, &mut market, request), Ok(41));
        assert_eq!(source.money, Some(40));
        assert!(source.item.is_none());
        assert!(source.hold.is_none(), "Hold is deleted last");
        assert_eq!(source.receipt.as_ref().unwrap().auction_id, 41);
        assert_eq!(market.auction_count, 1);
        assert_eq!(
            market.receipt.as_ref().unwrap().listing.snapshot,
            item(23).snapshot
        );

        assert_eq!(drive_sharded(&mut source, &mut market, request), Ok(41));
        assert_eq!(source.money, Some(40));
        assert_eq!(market.auction_count, 1);
    }
}

#[test]
fn sharded_listing_accounts_for_the_item_and_deposit_at_every_durable_boundary() {
    let mut source = source();
    let mut market = market();
    let request = request();

    assert_eq!(source.money, Some(50));
    assert_eq!(
        source.item.as_ref().map(|item| item.snapshot),
        Some(item(23).snapshot)
    );
    assert!(source.hold.is_none());
    assert!(market.receipt.is_none());

    fence_listing(&mut source, request).unwrap();
    let hold = source.hold(request.operation_id).unwrap();
    assert_eq!(source.money, Some(40));
    assert!(source.item.is_none());
    assert_eq!(hold.listing.snapshot, item(23).snapshot);
    assert_eq!(40 + hold.listing.deposit, 50);
    assert!(market.receipt.is_none());

    let auction_id = commit_held_listing(&mut market, hold.listing.clone()).unwrap();
    let market_receipt = market.receipt.as_ref().unwrap();
    assert_eq!(auction_id, 41);
    assert_eq!(market_receipt.listing.snapshot, item(23).snapshot);
    assert_eq!(40 + market_receipt.listing.deposit, 50);
    assert_eq!(
        source.hold.as_ref().unwrap().listing.snapshot,
        market_receipt.listing.snapshot,
        "the source Hold is recovery evidence for the one market item"
    );

    confirm_listing(
        &mut source,
        ListingReceipt {
            listing: hold.listing,
            auction_id,
        },
    )
    .unwrap();
    assert_eq!(
        source.receipt.as_ref().unwrap(),
        market.receipt.as_ref().unwrap()
    );
    assert!(source.hold.is_some());

    settle_listing(&mut source, request.operation_id).unwrap();
    assert!(source.hold.is_none());
    assert_eq!(source.money, Some(40));
    assert_eq!(market.auction_count, 1);
    assert_eq!(
        market.receipt.as_ref().unwrap().listing.snapshot,
        item(23).snapshot
    );
    assert_eq!(40 + market.receipt.as_ref().unwrap().listing.deposit, 50);
}

#[test]
fn conflicting_operation_id_reuse_fails_closed_on_both_planes() {
    let mut source = source();
    let mut market = market();
    let original = request();
    fence_listing(&mut source, original).unwrap();
    let hold = source.hold(original.operation_id).unwrap();
    commit_held_listing(&mut market, hold.listing).unwrap();

    let mut changed = original;
    changed.terms.buyout += 1;
    assert_eq!(
        fence_listing(&mut source, changed),
        Err(AuctionRefusal::InvalidTerms)
    );

    let mut changed_payload = source.hold(original.operation_id).unwrap().listing;
    changed_payload.snapshot.durability += 1;
    assert_eq!(
        commit_held_listing(&mut market, changed_payload),
        Err(AuctionRefusal::InvalidTerms)
    );
    assert_eq!(source.money, Some(40));
    assert_eq!(market.auction_count, 1);
}

#[test]
fn realm_refusal_releases_the_hold_and_mails_the_item_and_deposit_back() {
    let mut source = source();
    let mut realm = FakeRefundCore::default();
    let request = request();
    fence_listing(&mut source, request).unwrap();
    assert_eq!(source.money, Some(40));
    assert!(source.item.is_none());

    // Realm-core refused (for example, its item catalogue lacks the template), so
    // the market never took the listing and the source Hold is the only copy of the value.
    // The refund Mail and its receipt are written on Realm-core before its source Hold moves.
    let refund = listing_refund(&source.hold(request.operation_id).unwrap().listing);
    assert_eq!(refund_listing(&mut realm, refund.clone()), Ok(()));
    assert_eq!(realm.mails, vec![refund.mail]);
    assert!(source.hold.is_some());
    let mut changed_refund = refund.clone();
    changed_refund.listing.snapshot.durability += 1;
    assert_eq!(
        refund_listing(&mut realm, changed_refund),
        Err(AuctionRefusal::InvalidTerms),
        "the durable refund receipt refuses conflicting operation-id reuse"
    );
    assert_eq!(realm.mails.len(), 1);
    assert_eq!(
        release_listing(&mut source, request.operation_id, request.seller_guid + 1),
        Err(AuctionRefusal::InvalidTerms),
        "a wrong seller never releases another character's Hold"
    );
    assert!(source.hold.is_some());
    assert_eq!(
        release_listing(&mut source, request.operation_id, request.seller_guid),
        Ok(())
    );
    assert!(source.hold.is_none());

    // A gateway crash after Realm-core commits but before source cleanup replays one Mail.
    assert_eq!(refund_listing(&mut realm, refund), Ok(()));
    assert_eq!(realm.mails.len(), 1);
    assert_eq!(
        release_listing(&mut source, request.operation_id, request.seller_guid),
        Ok(()),
        "a replayed release finds no Hold and returns nothing twice"
    );
    assert_eq!(realm.mails.len(), 1);
}

#[test]
fn refund_receipt_replays_as_a_refusal_and_never_confirms_the_source_hold() {
    let mut source = source();
    let mut realm = market();
    let request = request();
    fence_listing(&mut source, request).unwrap();
    let listing = source.hold(request.operation_id).unwrap().listing;
    realm.receipt = Some(ListingReceipt {
        listing: listing.clone(),
        auction_id: 0,
    });

    assert_eq!(
        commit_held_listing(&mut realm, listing.clone()),
        Err(AuctionRefusal::InvalidTerms),
        "a Realm-core refund receipt is not an Auction"
    );
    let mut local = local();
    local.committed = Some((listing.clone(), 0));
    assert_eq!(
        operation_match(&local, listing.request),
        OperationMatch::Conflict,
        "a sentinel is never a local-listing replay"
    );
    source.receipt = Some(ListingReceipt {
        listing: listing.clone(),
        auction_id: 0,
    });
    assert_eq!(
        fence_listing(&mut source, listing.request),
        Err(AuctionRefusal::InvalidTerms),
        "a misrouted sentinel must fail closed on the source"
    );
    assert_eq!(
        confirm_listing(
            &mut source,
            ListingReceipt {
                listing,
                auction_id: 0,
            },
        ),
        Err(AuctionRefusal::InvalidTerms),
        "the sentinel must never cross back to the source"
    );
    assert_eq!(
        settle_listing(&mut source, request.operation_id),
        Err(AuctionRefusal::InvalidTerms),
        "source cleanup may not treat a sentinel as receipt evidence"
    );
    assert!(source.hold.is_some());
    assert_eq!(
        source.receipt.as_ref().map(|receipt| receipt.auction_id),
        Some(0)
    );
}

#[test]
fn release_refuses_a_hold_that_backs_a_live_auction() {
    let mut source = source();
    let mut market = market();
    let request = request();
    fence_listing(&mut source, request).unwrap();
    let hold = source.hold(request.operation_id).unwrap();
    let auction_id = commit_held_listing(&mut market, hold.listing.clone()).unwrap();
    confirm_listing(
        &mut source,
        ListingReceipt {
            listing: hold.listing,
            auction_id,
        },
    )
    .unwrap();

    assert_eq!(
        release_listing(&mut source, request.operation_id, request.seller_guid),
        Err(AuctionRefusal::InvalidTerms)
    );
    assert!(source.hold.is_some());
    assert_eq!(drive_sharded(&mut source, &mut market, request), Ok(41));
    assert!(source.hold.is_none());
    assert_eq!(market.auction_count, 1);
}

#[derive(Clone)]
struct FakeExpiry {
    auction: Option<ActiveAuction>,
    schedule_count: usize,
    mail: Vec<AuctionMail>,
    notices: Vec<AuctionNoticeDraft>,
    returned_items: Vec<crate::items::ItemSnapshot>,
    refunded_copper: u32,
}

impl ExpirySink for FakeExpiry {
    fn auction(&self, auction_id: u32) -> Result<Option<ActiveAuction>, String> {
        Ok(self
            .auction
            .as_ref()
            .filter(|auction| auction.id == auction_id)
            .cloned())
    }

    fn complete_expiry(&mut self, auction: ActiveAuction, completion: ExpiryCompletion) {
        let house = auction.listing.request.house.id;
        let item = auction.listing.snapshot;
        let owner_guid = auction.listing.request.seller_guid;
        match completion {
            ExpiryCompletion::Unsold(mail) => {
                self.returned_items.push(mail.attached_item);
                self.notices
                    .push(expired_notice(house, auction.id, item, owner_guid));
                self.mail.push(mail);
            }
            ExpiryCompletion::Sold(mail) => {
                self.notices.extend(settlement_notices(
                    house,
                    auction.id,
                    item.entry,
                    item.random_property_id,
                    owner_guid,
                    auction.highest_bidder_guid,
                    auction.highest_bid,
                ));
                self.mail.extend(mail);
            }
        }
        self.auction = None;
        self.schedule_count = 0;
    }
}

#[test]
fn replayed_unbid_expiry_returns_the_exact_item_once_and_forfeits_the_deposit() {
    let listing = PreparedListing {
        request: request(),
        snapshot: item(23).snapshot,
        deposit: 10,
        created_micros: 1_000,
        expires_micros: 7_200_001_000,
    };
    let mut store = FakeExpiry {
        auction: Some(ActiveAuction {
            id: 41,
            listing,
            highest_bidder_guid: 0,
            highest_bid: 0,
        }),
        schedule_count: 1,
        mail: Vec::new(),
        notices: Vec::new(),
        returned_items: Vec::new(),
        refunded_copper: 0,
    };

    assert_eq!(expire_active(&mut store, 41), Ok(()));
    assert_eq!(expire_active(&mut store, 41), Ok(()));

    assert!(store.auction.is_none());
    assert_eq!(store.schedule_count, 0);
    assert_eq!(store.returned_items, vec![item(23).snapshot]);
    assert_eq!(store.refunded_copper, 0, "the listing deposit is forfeited");
}

#[test]
fn local_and_sharded_listings_produce_the_same_unbid_and_bid_expiry_mail() {
    let mut local_store = local();
    create_local_listing(&mut local_store, request()).unwrap();
    let local_listing = local_store.committed.unwrap().0;

    let mut source = source();
    let mut market = market();
    drive_sharded(&mut source, &mut market, request()).unwrap();
    let sharded_listing = market.receipt.unwrap().listing;
    assert_eq!(local_listing, sharded_listing);

    let expire = |listing: PreparedListing, highest_bidder_guid, highest_bid| {
        let mut store = FakeExpiry {
            auction: Some(ActiveAuction {
                id: 41,
                listing,
                highest_bidder_guid,
                highest_bid,
            }),
            schedule_count: 1,
            mail: Vec::new(),
            notices: Vec::new(),
            returned_items: Vec::new(),
            refunded_copper: 0,
        };
        expire_active(&mut store, 41).unwrap();
        store
    };
    for (highest_bidder_guid, highest_bid) in [(0, 0), (8, 201)] {
        let local_expiry = expire(local_listing.clone(), highest_bidder_guid, highest_bid);
        let sharded_expiry = expire(sharded_listing.clone(), highest_bidder_guid, highest_bid);
        assert_eq!(local_expiry.mail, sharded_expiry.mail);
        assert_eq!(local_expiry.returned_items, sharded_expiry.returned_items);
        assert_eq!(local_expiry.refunded_copper, sharded_expiry.refunded_copper);
        assert_eq!(local_expiry.schedule_count, sharded_expiry.schedule_count);
    }
}

#[test]
fn replayed_bid_expiry_mails_the_exact_item_and_imported_rate_proceeds_once() {
    let listing = PreparedListing {
        request: request(),
        snapshot: item(23).snapshot,
        deposit: 10,
        created_micros: 1_000,
        expires_micros: 7_200_001_000,
    };
    let mut store = FakeExpiry {
        auction: Some(ActiveAuction {
            id: 41,
            listing,
            highest_bidder_guid: 8,
            highest_bid: 201,
        }),
        schedule_count: 1,
        mail: Vec::new(),
        notices: Vec::new(),
        returned_items: Vec::new(),
        refunded_copper: 0,
    };

    assert_eq!(expire_active(&mut store, 41), Ok(()));
    assert_eq!(expire_active(&mut store, 41), Ok(()));

    assert!(store.auction.is_none());
    assert_eq!(store.schedule_count, 0);
    assert_eq!(store.returned_items, Vec::new());
    assert_eq!(
        store.mail,
        vec![
            AuctionMail {
                recipient_guid: 8,
                house: 1,
                action: AuctionMailAction::Won,
                item_entry: 25,
                random_property_id: 117,
                money: 0,
                attached_item: item(23).snapshot,
                counterparty_guid: 7,
                bid: 201,
                buyout: 20,
                deposit: 0,
                cut: 0,
            },
            AuctionMail {
                recipient_guid: 7,
                house: 1,
                action: AuctionMailAction::Successful,
                item_entry: 25,
                random_property_id: 117,
                money: 201,
                attached_item: crate::items::ItemSnapshot::default(),
                counterparty_guid: 8,
                bid: 201,
                buyout: 20,
                deposit: 10,
                cut: 10,
            },
        ],
        "one Sold mail settles at the LISTING's buyout term, not the expiring bid"
    );
    assert_eq!(
        store.notices,
        vec![
            AuctionNoticeDraft {
                recipient_guid: 8,
                kind: auction_notice::WON,
                house: 1,
                auction_id: 41,
                item_entry: 25,
                random_property_id: 117,
                bid: 0,
                out_bid: 11, // vanilla minimum raise on a 201 bid: 5% rounded up
                bidder_guid: 8,
            },
            AuctionNoticeDraft {
                recipient_guid: 7,
                kind: auction_notice::SOLD,
                house: 1,
                auction_id: 41,
                item_entry: 25,
                random_property_id: 117,
                bid: 201,
                out_bid: 11, // vanilla minimum raise on a 201 bid: 5% rounded up
                bidder_guid: 0,
            },
        ],
        "a replayed expiry callback must not write a second notice"
    );
}

#[test]
fn realm_bid_decision_enforces_the_vanilla_price_ladder_and_revision() {
    let active = active_bid_auction();
    let request = HoldRequest {
        operation: HoldOperation::Bid,
        operation_id: 900,
        bidder_guid: 8,
        auction_id: 41,
        house: 1,
        offer: 100,
    };

    assert_eq!(
        decide_bid(Some(active), request, 1_000),
        accepted_active(100, 4, 0, 0)
    );

    let bid = BidAuction {
        highest_bidder_guid: 9,
        highest_bid: 101,
        ..active
    };
    assert_eq!(minimum_next_bid(bid), Ok(107));
    assert_eq!(
        decide_bid(
            Some(bid),
            HoldRequest {
                offer: 101,
                ..request
            },
            1_000
        ),
        HoldDecision::HigherBid {
            bidder_guid: 9,
            current_bid: 101,
            minimum_increment: 6,
        }
    );
    assert_eq!(
        decide_bid(
            Some(bid),
            HoldRequest {
                offer: 106,
                ..request
            },
            1_000
        ),
        HoldDecision::BidIncrement
    );
    assert_eq!(
        decide_bid(
            Some(active),
            HoldRequest {
                offer: 99,
                ..request
            },
            1_000
        ),
        HoldDecision::BidIncrement
    );
    assert_eq!(
        decide_bid(
            Some(BidAuction {
                owner_guid: 8,
                ..active
            }),
            request,
            1_000,
        ),
        HoldDecision::BidOwn
    );
    assert_eq!(decide_bid(None, request, 1_000), HoldDecision::ItemNotFound);
    assert_eq!(
        decide_bid(
            Some(active),
            HoldRequest {
                house: 7,
                ..request
            },
            1_000,
        ),
        HoldDecision::ItemNotFound
    );
    assert_eq!(
        decide_bid(
            Some(BidAuction {
                expires_micros: 1_000,
                ..active
            }),
            request,
            1_000,
        ),
        HoldDecision::ItemNotFound
    );
    assert_eq!(
        decide_bid(
            Some(active),
            HoldRequest {
                operation_id: 0,
                ..request
            },
            1_000
        ),
        HoldDecision::Database
    );
    assert!(matches!(
        decide_bid(
            Some(BidAuction {
                highest_bid: u32::MAX,
                buyout: 0,
                ..bid
            }),
            request,
            1_000,
        ),
        HoldDecision::Database
    ));
}

/// The seven imported houses pool into three markets: a bid through any Alliance house
/// reaches a listing at any Alliance house, but never a Horde or neutral one.
#[test]
fn realm_bid_decision_accepts_a_bid_within_the_listings_market_and_refuses_outside_it() {
    let active = active_bid_auction(); // lists in house 1 (Stormwind, Alliance).
    let request = HoldRequest {
        operation: HoldOperation::Bid,
        operation_id: 900,
        bidder_guid: 8,
        auction_id: 41,
        house: 2, // a different Alliance house.
        offer: 100,
    };

    assert_eq!(
        decide_bid(Some(active), request, 1_000),
        accepted_active(100, 4, 0, 0)
    );
    assert_eq!(
        decide_bid(
            Some(active),
            HoldRequest {
                house: 3,
                ..request
            },
            1_000
        ),
        accepted_active(100, 4, 0, 0)
    );
    for horde_house in [4, 5, 6] {
        assert_eq!(
            decide_bid(
                Some(active),
                HoldRequest {
                    house: horde_house,
                    ..request
                },
                1_000
            ),
            HoldDecision::ItemNotFound
        );
    }
    assert_eq!(
        decide_bid(
            Some(active),
            HoldRequest {
                house: 7,
                ..request
            },
            1_000
        ),
        HoldDecision::ItemNotFound
    );
}

#[test]
fn realm_buyout_normalizes_the_offer_and_checks_settlement_arithmetic() {
    let active = BidAuction {
        highest_bidder_guid: 9,
        highest_bid: 201,
        buyout: 500,
        ..active_bid_auction()
    };
    let request = HoldRequest {
        operation: HoldOperation::Bid,
        operation_id: 905,
        bidder_guid: 8,
        auction_id: 41,
        house: 1,
        offer: 900,
    };

    for offer in [500, 900] {
        assert_eq!(
            decide_bid(Some(active), HoldRequest { offer, ..request }, 1_000),
            accepted_buyout(500, 9, 201)
        );
    }
    assert_eq!(
        decide_bid(
            Some(BidAuction {
                deposit: u32::MAX,
                buyout: u32::MAX,
                ..active
            }),
            HoldRequest {
                offer: u32::MAX,
                ..request
            },
            1_000,
        ),
        HoldDecision::Database
    );
    assert!(matches!(
        decide_bid(
            Some(BidAuction {
                highest_bidder_guid: 9,
                highest_bid: u32::MAX - 1,
                buyout: u32::MAX,
                deposit: 1,
                ..active
            }),
            HoldRequest {
                offer: u32::MAX,
                ..request
            },
            1_000,
        ),
        HoldDecision::Accepted(BidAcceptance {
            price: u32::MAX,
            effect: AuctionBidEffect::SettleBuyout,
            ..
        })
    ));
}

#[derive(Clone, Copy)]
struct FakeBidSource {
    money: u32,
    deferred_refund: u32,
    hold: Option<HeldBid>,
}

impl FakeBidSource {
    fn new(money: u32) -> Self {
        Self {
            money,
            deferred_refund: 0,
            hold: None,
        }
    }
}

impl BidSource for FakeBidSource {
    fn money(&self, _bidder_guid: u64) -> Option<u32> {
        Some(self.money)
    }

    fn hold(&self, operation_id: u64) -> Option<HeldBid> {
        self.hold
            .filter(|hold| hold.request.operation_id == operation_id)
    }

    fn create_hold(&mut self, request: HoldRequest) -> Result<(), AuctionRefusal> {
        self.money -= request.offer;
        self.hold = Some(HeldBid {
            request,
            decision: None,
            deferred_refund: 0,
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
        if refund != 0 {
            let (money, deferred_refund) = crate::mail::split_refund(self.money, refund);
            self.money = money;
            self.deferred_refund += deferred_refund;
        }
        self.hold = Some(HeldBid {
            request,
            decision: Some(decision),
            deferred_refund: self.deferred_refund,
        });
        Ok(())
    }

    fn confirm_refund(&mut self, request: HoldRequest) -> Result<(), AuctionRefusal> {
        self.deferred_refund = 0;
        self.hold = self.hold.map(|mut hold| {
            assert_eq!(hold.request, request);
            hold.deferred_refund = 0;
            hold
        });
        Ok(())
    }
}

#[test]
fn bid_hold_decisions_are_terminal_replay_safe_and_conserve_copper() {
    let request = HoldRequest {
        operation: HoldOperation::Bid,
        operation_id: 901,
        bidder_guid: 8,
        auction_id: 41,
        house: 1,
        offer: 107,
    };
    let rejected = HoldDecision::BidIncrement;
    let accepted = accepted_active(107, 4, 9, 101);

    let mut rejection = FakeBidSource::new(200);
    assert_eq!(fence_bid(&mut rejection, request), Ok(()));
    assert_eq!(rejection.money, 93, "the full offer is held first");
    assert_eq!(finish_bid(&mut rejection, request, rejected), Ok(rejected));
    assert_eq!(rejection.money, 200, "a rejection restores the offer");
    assert_eq!(finish_bid(&mut rejection, request, rejected), Ok(rejected));
    assert_eq!(rejection.money, 200, "replay cannot restore twice");

    let mut overflowed_rejection = FakeBidSource::new(200);
    assert_eq!(fence_bid(&mut overflowed_rejection, request), Ok(()));
    overflowed_rejection.money = u32::MAX;
    assert_eq!(
        finish_bid(&mut overflowed_rejection, request, rejected),
        Ok(rejected),
        "an intervening purse credit cannot strand a rejected Hold"
    );
    assert_eq!(overflowed_rejection.money, u32::MAX);
    assert_eq!(overflowed_rejection.deferred_refund, request.offer);
    assert_eq!(
        finish_bid(&mut overflowed_rejection, request, rejected),
        Ok(rejected)
    );
    assert_eq!(
        overflowed_rejection.deferred_refund, request.offer,
        "replay cannot defer the refund twice"
    );
    assert_eq!(
        confirm_bid_refund(&mut overflowed_rejection, request, request.offer),
        Ok(())
    );
    assert_eq!(overflowed_rejection.deferred_refund, 0);
    assert_eq!(
        confirm_bid_refund(&mut overflowed_rejection, request, request.offer),
        Ok(()),
        "confirmation replay is idempotent"
    );

    let mut acceptance = FakeBidSource::new(200);
    assert_eq!(fence_bid(&mut acceptance, request), Ok(()));
    assert_eq!(finish_bid(&mut acceptance, request, accepted), Ok(accepted));
    assert_eq!(finish_bid(&mut acceptance, request, accepted), Ok(accepted));
    assert_eq!(acceptance.money, 93, "accepted value is consumed once");

    let normalized = accepted_active(100, 4, 9, 101);
    let mut normalized_acceptance = FakeBidSource::new(200);
    assert_eq!(fence_bid(&mut normalized_acceptance, request), Ok(()));
    assert_eq!(
        finish_bid(&mut normalized_acceptance, request, normalized),
        Ok(normalized)
    );
    assert_eq!(normalized_acceptance.money, 100);
    assert_eq!(
        finish_bid(&mut normalized_acceptance, request, normalized),
        Ok(normalized)
    );
    assert_eq!(normalized_acceptance.money, 100);

    let mut interrupted_normalization = FakeBidSource::new(200);
    fence_bid(&mut interrupted_normalization, request).unwrap();
    interrupted_normalization.money = u32::MAX;
    finish_bid(&mut interrupted_normalization, request, normalized).unwrap();
    assert_eq!(interrupted_normalization.deferred_refund, 7);
    assert_eq!(
        confirm_bid_refund(&mut interrupted_normalization, request, 7),
        Ok(())
    );
    assert_eq!(interrupted_normalization.deferred_refund, 0);

    assert_eq!(
        finish_bid(
            &mut acceptance,
            HoldRequest {
                offer: 108,
                ..request
            },
            accepted
        ),
        Err(AuctionRefusal::Database),
        "changed-payload identifier reuse fails closed"
    );

    for malformed in [
        HoldRequest {
            operation_id: 0,
            ..request
        },
        HoldRequest {
            bidder_guid: 0,
            ..request
        },
        HoldRequest {
            auction_id: 0,
            ..request
        },
        HoldRequest {
            offer: 0,
            ..request
        },
    ] {
        let mut source = FakeBidSource::new(200);
        assert_eq!(
            fence_bid(&mut source, malformed),
            Err(AuctionRefusal::Database)
        );
        assert_eq!(source.money, 200);
        assert!(source.hold.is_none());
    }
    let mut poor = FakeBidSource::new(106);
    assert_eq!(
        fence_bid(&mut poor, request),
        Err(AuctionRefusal::NotEnoughMoney)
    );
    assert_eq!(poor.money, 106);
    assert!(poor.hold.is_none());
}

#[derive(Clone)]
struct FakeBidMarket {
    auction: Option<BidAuction>,
    decisions: Vec<(HoldRequest, HoldDecision)>,
    mail: Vec<AuctionMail>,
    notices: Vec<AuctionNoticeDraft>,
    expiry_armed: bool,
    now_micros: i64,
}

impl FakeBidMarket {
    fn mailbox(&self, recipient_guid: u64) -> Vec<AuctionMail> {
        self.mail
            .iter()
            .copied()
            .filter(|mail| mail.recipient_guid == recipient_guid)
            .collect()
    }
}

impl BidMarket for FakeBidMarket {
    fn decision(&self, operation_id: u64) -> Option<(HoldRequest, HoldDecision)> {
        self.decisions
            .iter()
            .copied()
            .find(|(request, _)| request.operation_id == operation_id)
    }

    fn auction(&self, auction_id: u32) -> Option<BidAuction> {
        self.auction.filter(|auction| auction.id == auction_id)
    }

    fn now_micros(&self) -> i64 {
        self.now_micros
    }

    fn commit_decision(
        &mut self,
        request: HoldRequest,
        auction: Option<BidAuction>,
        decision: HoldDecision,
    ) -> Result<(), AuctionRefusal> {
        if let HoldDecision::Accepted(accepted) = decision {
            let mut auction = auction.expect("only an active Auction can accept a bid");
            self.mail.extend(displaced_bid_refund_mail(
                auction.house,
                auction.item,
                accepted.displaced_bidder_guid,
                accepted.displaced_bid,
            ));
            self.notices.extend(outbid_notice(
                auction.house,
                request.auction_id,
                auction.item,
                accepted.displaced_bidder_guid,
                request.bidder_guid,
                accepted.displaced_bid,
            ));
            match accepted.effect {
                AuctionBidEffect::SettleBuyout => {
                    self.expiry_armed = false;
                    self.mail.extend(
                        buyout_settlement_mail(auction, request.bidder_guid, accepted.price)
                            .expect("accepted buyout arithmetic was checked"),
                    );
                    self.notices.extend(settlement_notices(
                        auction.house,
                        request.auction_id,
                        auction.item.entry,
                        auction.item.random_property_id,
                        auction.owner_guid,
                        request.bidder_guid,
                        accepted.price,
                    ));
                    self.auction = None;
                }
                AuctionBidEffect::RemainActive { revision } => {
                    self.notices.push(new_bid_notice(
                        auction.house,
                        request.auction_id,
                        auction.item,
                        auction.owner_guid,
                        request.bidder_guid,
                        accepted.price,
                    ));
                    auction.highest_bidder_guid = request.bidder_guid;
                    auction.highest_bid = accepted.price;
                    auction.revision = revision;
                    self.auction = Some(auction);
                }
            }
        }
        if let HoldDecision::Cancelled {
            displaced_bidder_guid,
            displaced_bid,
            ..
        } = decision
        {
            let auction = auction.expect("only an active Auction can be cancelled");
            self.mail.extend(cancellation_mail(
                auction,
                displaced_bidder_guid,
                displaced_bid,
            ));
            self.notices.extend(removed_notice(
                auction.house,
                auction.id,
                auction.item,
                displaced_bidder_guid,
                displaced_bid,
            ));
            self.auction = None;
            self.expiry_armed = false;
        }
        self.decisions.push((request, decision));
        Ok(())
    }
}

struct BidMarketExpiry<'a> {
    market: &'a mut FakeBidMarket,
    listing: PreparedListing,
}

impl ExpirySink for BidMarketExpiry<'_> {
    fn auction(&self, auction_id: u32) -> Result<Option<ActiveAuction>, String> {
        Ok(self
            .market
            .auction
            .filter(|auction| auction.id == auction_id)
            .map(|auction| ActiveAuction {
                id: auction.id,
                listing: self.listing.clone(),
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
                self.market
                    .notices
                    .push(expired_notice(house, auction.id, item, owner_guid));
                self.market.mail.push(mail);
            }
            ExpiryCompletion::Sold(mail) => {
                self.market.notices.extend(settlement_notices(
                    house,
                    auction.id,
                    item.entry,
                    item.random_property_id,
                    owner_guid,
                    auction.highest_bidder_guid,
                    auction.highest_bid,
                ));
                self.market.mail.extend(mail);
            }
        }
        self.market.auction = None;
        self.market.expiry_armed = false;
    }
}

fn expire_bid_market(market: &mut FakeBidMarket, listing: PreparedListing) -> Result<(), String> {
    let auction_id = market.auction.map_or(41, |auction| auction.id);
    expire_active(&mut BidMarketExpiry { market, listing }, auction_id)
}

struct FakeBidRefundSink {
    request: HoldRequest,
    decision: HoldDecision,
    recorded: u32,
    mails: Vec<(u64, u32)>,
}

impl BidRefundSink for FakeBidRefundSink {
    fn refund_decision(&self, operation_id: u64) -> Option<(HoldRequest, HoldDecision, u32)> {
        (self.request.operation_id == operation_id).then_some((
            self.request,
            self.decision,
            self.recorded,
        ))
    }

    fn commit_refund(&mut self, request: HoldRequest, amount: u32) -> Result<(), AuctionRefusal> {
        self.recorded = amount;
        self.mails.push((request.bidder_guid, amount));
        Ok(())
    }
}

#[test]
fn deferred_bid_refund_relay_is_terminal_and_payload_safe() {
    let request = HoldRequest {
        operation: HoldOperation::Bid,
        operation_id: 904,
        bidder_guid: 8,
        auction_id: 41,
        house: 1,
        offer: 107,
    };
    let mut sink = FakeBidRefundSink {
        request,
        decision: HoldDecision::BidIncrement,
        recorded: 0,
        mails: Vec::new(),
    };

    assert_eq!(relay_bid_refund(&mut sink, request, 7), Ok(()));
    assert_eq!(relay_bid_refund(&mut sink, request, 7), Ok(()));
    assert_eq!(sink.mails, vec![(8, 7)]);
    assert_eq!(
        relay_bid_refund(
            &mut sink,
            HoldRequest {
                offer: 108,
                ..request
            },
            7
        ),
        Err(AuctionRefusal::Database)
    );
    assert_eq!(
        relay_bid_refund(&mut sink, request, 8),
        Err(AuctionRefusal::Database)
    );

    sink.recorded = 0;
    sink.decision = accepted_active(100, 4, 9, 101);
    assert_eq!(relay_bid_refund(&mut sink, request, 7), Ok(()));
    assert_eq!(relay_bid_refund(&mut sink, request, 7), Ok(()));
    assert_eq!(sink.mails, vec![(8, 7), (8, 7)]);
    assert_eq!(
        relay_bid_refund(&mut sink, request, 8),
        Err(AuctionRefusal::Database)
    );
}

#[test]
fn realm_bid_replay_updates_once_and_returns_the_displaced_bid_once() {
    let request = HoldRequest {
        operation: HoldOperation::Bid,
        operation_id: 902,
        bidder_guid: 8,
        auction_id: 41,
        house: 1,
        offer: 107,
    };
    let mut market = FakeBidMarket {
        auction: Some(BidAuction {
            highest_bidder_guid: 9,
            highest_bid: 101,
            ..active_bid_auction()
        }),
        decisions: Vec::new(),
        mail: Vec::new(),
        notices: Vec::new(),
        expiry_armed: true,
        now_micros: 1_000,
    };

    let expected = accepted_active(107, 4, 9, 101);
    assert_eq!(resolve_bid(&mut market, request), Ok(expected));
    assert_eq!(resolve_bid(&mut market, request), Ok(expected));
    assert_eq!(
        market.mail,
        vec![displaced_bid_refund_mail(1, item(23).snapshot, 9, 101).unwrap()]
    );
    assert_eq!(
        market.notices,
        vec![
            outbid_notice(1, 41, item(23).snapshot, 9, 8, 101).unwrap(),
            new_bid_notice(1, 41, item(23).snapshot, 7, 8, 107),
        ],
        "the replayed decision must not write a second Outbid or New bid notice"
    );
    assert_eq!(
        market.auction.map(|auction| (
            auction.highest_bidder_guid,
            auction.highest_bid,
            auction.revision,
        )),
        Some((8, 107, 4))
    );

    assert_eq!(
        resolve_bid(
            &mut market,
            HoldRequest {
                offer: 108,
                ..request
            }
        ),
        Err(AuctionRefusal::Database)
    );
    assert_eq!(
        market.mail,
        vec![displaced_bid_refund_mail(1, item(23).snapshot, 9, 101).unwrap()]
    );
}

#[test]
fn local_and_interrupted_sharded_bids_and_buyouts_have_equivalent_state() {
    for (operation_id, offer) in [(903, 107), (904, 900)] {
        let request = HoldRequest {
            operation: HoldOperation::Bid,
            operation_id,
            bidder_guid: 8,
            auction_id: 41,
            house: 1,
            offer,
        };
        let source = FakeBidSource::new(1_000);
        let market = FakeBidMarket {
            auction: Some(BidAuction {
                highest_bidder_guid: 9,
                highest_bid: 101,
                buyout: 500,
                ..active_bid_auction()
            }),
            decisions: Vec::new(),
            mail: Vec::new(),
            notices: Vec::new(),
            expiry_armed: true,
            now_micros: 1_000,
        };

        let mut local_source = source;
        let mut local_market = market.clone();
        let expected = drive_bid(&mut local_source, &mut local_market, request).unwrap();

        for killed_after in 0..=2 {
            let mut sharded_source = source;
            let mut sharded_market = market.clone();
            if killed_after >= 1 {
                fence_bid(&mut sharded_source, request).unwrap();
            }
            if killed_after >= 2 {
                resolve_bid(&mut sharded_market, request).unwrap();
            }

            assert_eq!(
                drive_bid(&mut sharded_source, &mut sharded_market, request),
                Ok(expected)
            );
            assert_eq!(sharded_source.money, local_source.money);
            assert_eq!(sharded_source.hold, local_source.hold);
            assert_eq!(sharded_market.auction, local_market.auction);
            assert_eq!(sharded_market.decisions, local_market.decisions);
            assert_eq!(sharded_market.mail, local_market.mail);
            assert_eq!(sharded_market.expiry_armed, local_market.expiry_armed);
            assert_eq!(
                drive_bid(&mut sharded_source, &mut sharded_market, request),
                Ok(expected)
            );
            assert_eq!(sharded_market.decisions, local_market.decisions);
            assert_eq!(sharded_market.mail, local_market.mail);
        }
    }
}

fn listing_for_flow(sharded: bool, operation_id: u64) -> PreparedListing {
    let request = ListingRequest {
        operation_id,
        ..request()
    };
    if sharded {
        let mut source = source();
        let mut market = market();
        fence_listing(&mut source, request).unwrap();
        assert_eq!(drive_sharded(&mut source, &mut market, request), Ok(41));
        assert_eq!(drive_sharded(&mut source, &mut market, request), Ok(41));
        market.receipt.unwrap().listing
    } else {
        let mut store = local();
        assert_eq!(create_local_listing(&mut store, request), Ok(41));
        assert_eq!(create_local_listing(&mut store, request), Ok(41));
        store.committed.unwrap().0
    }
}

fn bid_market_for(listing: &PreparedListing) -> FakeBidMarket {
    FakeBidMarket {
        auction: Some(BidAuction {
            id: 41,
            house: listing.request.house.id,
            owner_guid: listing.request.seller_guid,
            item: listing.snapshot,
            highest_bidder_guid: 0,
            highest_bid: 0,
            start_bid: listing.request.terms.start_bid,
            buyout: listing.request.terms.buyout,
            deposit: listing.deposit,
            consignment_rate: listing.request.house.consignment_rate,
            expires_micros: listing.expires_micros,
            revision: 0,
        }),
        decisions: Vec::new(),
        mail: Vec::new(),
        notices: Vec::new(),
        expiry_armed: true,
        now_micros: listing.created_micros,
    }
}

fn bid_for_flow(
    sharded: bool,
    source: &mut FakeBidSource,
    market: &mut FakeBidMarket,
    request: HoldRequest,
) -> HoldDecision {
    if sharded {
        fence_bid(source, request).unwrap();
        resolve_bid(market, request).unwrap();
    }
    let decision = drive_bid(source, market, request).unwrap();
    assert_eq!(drive_bid(source, market, request), Ok(decision));
    decision
}

#[derive(Debug, PartialEq, Eq)]
struct CompleteFlowOutcome {
    browse_search_rows: Vec<BidAuction>,
    decisions: Vec<HoldDecision>,
    collected_mail: Vec<AuctionMail>,
    collected_copper: Vec<(u64, u32)>,
    collected_items: Vec<(u64, crate::items::ItemSnapshot)>,
    bidder_money: Vec<u32>,
}

fn collect_flow_mail(outcome: &mut CompleteFlowOutcome, mail: Vec<AuctionMail>) {
    for mail in mail {
        if mail.money != 0 {
            assert_eq!(
                crate::mail::plan_take_money(
                    Some((mail.recipient_guid, mail.money)),
                    mail.recipient_guid,
                ),
                crate::mail::TakeMoney::Take(mail.money),
            );
            assert_eq!(
                crate::mail::plan_take_money(Some((mail.recipient_guid, 0)), mail.recipient_guid,),
                crate::mail::TakeMoney::NothingToTake,
            );
            outcome
                .collected_copper
                .push((mail.recipient_guid, crate::mail::credited(0, mail.money)));
        }
        if !mail.attached_item.is_empty() {
            assert_eq!(
                crate::mail::plan_take_item(
                    Some((mail.recipient_guid, mail.attached_item.entry)),
                    mail.recipient_guid,
                ),
                crate::mail::TakeItem::Take,
            );
            assert_eq!(
                crate::mail::plan_take_item(Some((mail.recipient_guid, 0)), mail.recipient_guid,),
                crate::mail::TakeItem::NothingToTake,
            );
            outcome
                .collected_items
                .push((mail.recipient_guid, mail.attached_item));
        }
        outcome.collected_mail.push(mail);
    }
}

fn complete_flow(sharded: bool) -> CompleteFlowOutcome {
    let buyout_listing = listing_for_flow(sharded, 910);
    let mut buyout_market = bid_market_for(&buyout_listing);
    // Browse/search renders the same authoritative Auction row that the later writes consume.
    let browse_search_rows = buyout_market
        .auction
        .into_iter()
        .filter(|auction| auction.item.entry == item(23).snapshot.entry)
        .collect();
    let bid = HoldRequest {
        operation: HoldOperation::Bid,
        operation_id: 911,
        bidder_guid: 8,
        auction_id: 41,
        house: 1,
        offer: 10,
    };
    let mut bidder = FakeBidSource::new(100);
    let bid_decision = bid_for_flow(sharded, &mut bidder, &mut buyout_market, bid);
    let buyout = HoldRequest {
        operation: HoldOperation::Bid,
        operation_id: 912,
        bidder_guid: 9,
        auction_id: 41,
        house: 1,
        offer: 50,
    };
    let mut buyer = FakeBidSource::new(100);
    let buyout_decision = bid_for_flow(sharded, &mut buyer, &mut buyout_market, buyout);
    expire_bid_market(&mut buyout_market, buyout_listing.clone()).unwrap();
    let mut outcome = CompleteFlowOutcome {
        browse_search_rows,
        decisions: vec![bid_decision, buyout_decision],
        collected_mail: Vec::new(),
        collected_copper: Vec::new(),
        collected_items: Vec::new(),
        bidder_money: vec![bidder.money, buyer.money],
    };
    collect_flow_mail(&mut outcome, std::mem::take(&mut buyout_market.mail));

    let unbid_listing = listing_for_flow(sharded, 913);
    let mut unbid_market = bid_market_for(&unbid_listing);
    expire_bid_market(&mut unbid_market, unbid_listing.clone()).unwrap();
    expire_bid_market(&mut unbid_market, unbid_listing).unwrap();
    collect_flow_mail(&mut outcome, std::mem::take(&mut unbid_market.mail));

    let bid_expiry_listing = listing_for_flow(sharded, 914);
    let mut bid_expiry_market = bid_market_for(&bid_expiry_listing);
    let expiry_bid = HoldRequest {
        operation: HoldOperation::Bid,
        operation_id: 915,
        bidder_guid: 8,
        auction_id: 41,
        house: 1,
        offer: 10,
    };
    let mut expiry_bidder = FakeBidSource::new(100);
    let expiry_bid_decision = bid_for_flow(
        sharded,
        &mut expiry_bidder,
        &mut bid_expiry_market,
        expiry_bid,
    );
    expire_bid_market(&mut bid_expiry_market, bid_expiry_listing.clone()).unwrap();
    expire_bid_market(&mut bid_expiry_market, bid_expiry_listing).unwrap();
    collect_flow_mail(&mut outcome, std::mem::take(&mut bid_expiry_market.mail));
    outcome.decisions.push(expiry_bid_decision);
    outcome.bidder_money.push(expiry_bidder.money);
    outcome
}

#[test]
fn complete_flow_is_equivalent_across_local_and_interrupted_sharded_topologies() {
    let local = complete_flow(false);
    let sharded = complete_flow(true);
    assert_eq!(sharded, local);
    assert_eq!(local.browse_search_rows.len(), 1);
    assert_eq!(local.collected_mail.len(), 6);
    assert_eq!(
        local
            .collected_mail
            .iter()
            .map(|mail| (mail.recipient_guid, mail.action, mail.money))
            .collect::<Vec<_>>(),
        vec![
            (8, AuctionMailAction::Outbid, 10),
            (9, AuctionMailAction::Won, 0),
            (7, AuctionMailAction::Successful, 29),
            (7, AuctionMailAction::Expired, 0),
            (8, AuctionMailAction::Won, 0),
            (7, AuctionMailAction::Successful, 20),
        ]
    );
    assert_eq!(
        local.collected_items,
        vec![
            (9, item(23).snapshot),
            (7, item(23).snapshot),
            (8, item(23).snapshot),
        ],
        "buyout, unbid expiry, and bid expiry each collect one exact item"
    );
    assert_eq!(local.collected_copper, vec![(8, 10), (7, 29), (7, 20)]);
    assert_eq!(local.bidder_money, vec![90, 80, 90]);
}

#[test]
fn buyout_atomically_delivers_exact_mail_and_conserves_copper() {
    let request = HoldRequest {
        operation: HoldOperation::Bid,
        operation_id: 906,
        bidder_guid: 8,
        auction_id: 41,
        house: 1,
        offer: 900,
    };
    let mut source = FakeBidSource::new(1_000);
    let mut market = FakeBidMarket {
        auction: Some(BidAuction {
            highest_bidder_guid: 9,
            highest_bid: 201,
            buyout: 500,
            ..active_bid_auction()
        }),
        decisions: Vec::new(),
        mail: Vec::new(),
        notices: Vec::new(),
        expiry_armed: true,
        now_micros: 1_000,
    };

    assert_eq!(
        drive_bid(&mut source, &mut market, request),
        Ok(accepted_buyout(500, 9, 201))
    );
    assert_eq!(source.money, 500);
    assert!(market.auction.is_none());
    assert!(!market.expiry_armed);
    assert_eq!(
        market.mailbox(9),
        vec![displaced_bid_refund_mail(1, item(23).snapshot, 9, 201).unwrap()]
    );
    let winner_mail = market.mailbox(8);
    assert_eq!(winner_mail.len(), 1, "winner mail is visible immediately");
    assert_eq!(winner_mail[0].action, AuctionMailAction::Won);
    assert_eq!(winner_mail[0].counterparty_guid, 7);
    assert_eq!(winner_mail[0].money, 0);
    assert_eq!(winner_mail[0].attached_item, item(23).snapshot);
    let seller_mail = market.mailbox(7);
    assert_eq!(seller_mail.len(), 1, "seller mail is visible immediately");
    assert_eq!(seller_mail[0].action, AuctionMailAction::Successful);
    assert_eq!(seller_mail[0].counterparty_guid, 8);
    assert_eq!(seller_mail[0].money, 485);
    assert!(seller_mail[0].attached_item.is_empty());
    assert_eq!(market.mail.len(), 3);
    assert_eq!(1_000_u64 + 201 + 10, 500_u64 + 201 + 485 + 25);
    let [won_notice, sold_notice] = settlement_notices(1, 41, 25, 117, 7, 8, 500);
    assert_eq!(
        market.notices,
        vec![
            outbid_notice(1, 41, item(23).snapshot, 9, 8, 201).unwrap(),
            won_notice,
            sold_notice,
        ],
        "SettleBuyout writes Outbid, then Won and Sold — never New bid"
    );

    assert_eq!(
        drive_bid(&mut source, &mut market, request),
        Ok(accepted_buyout(500, 9, 201))
    );
    assert_eq!(source.money, 500);
    assert_eq!(market.mail.len(), 3);
    assert_eq!(
        market.notices.len(),
        3,
        "a replayed buyout writes no second notice"
    );
}

#[test]
fn concurrent_buyouts_have_one_winner_and_restore_the_loser_once() {
    let winner = HoldRequest {
        operation: HoldOperation::Bid,
        operation_id: 907,
        bidder_guid: 8,
        auction_id: 41,
        house: 1,
        offer: 900,
    };
    let loser = HoldRequest {
        operation: HoldOperation::Bid,
        operation_id: 908,
        bidder_guid: 10,
        auction_id: 41,
        house: 1,
        offer: 500,
    };
    let mut winner_source = FakeBidSource::new(1_000);
    let mut loser_source = FakeBidSource::new(700);
    let mut market = FakeBidMarket {
        auction: Some(BidAuction {
            buyout: 500,
            ..active_bid_auction()
        }),
        decisions: Vec::new(),
        mail: Vec::new(),
        notices: Vec::new(),
        expiry_armed: true,
        now_micros: 1_000,
    };

    fence_bid(&mut winner_source, winner).unwrap();
    fence_bid(&mut loser_source, loser).unwrap();
    let winner_decision = resolve_bid(&mut market, winner).unwrap();
    let loser_decision = resolve_bid(&mut market, loser).unwrap();
    assert!(matches!(
        winner_decision,
        HoldDecision::Accepted(BidAcceptance {
            price: 500,
            effect: AuctionBidEffect::SettleBuyout,
            ..
        })
    ));
    assert_eq!(loser_decision, HoldDecision::ItemNotFound);
    finish_bid(&mut winner_source, winner, winner_decision).unwrap();
    finish_bid(&mut loser_source, loser, loser_decision).unwrap();
    assert_eq!(winner_source.money, 500);
    assert_eq!(loser_source.money, 700);
    assert_eq!(market.mail.len(), 2);
    assert_eq!(market.decisions.len(), 2);

    assert_eq!(
        drive_bid(&mut loser_source, &mut market, loser),
        Ok(HoldDecision::ItemNotFound)
    );
    assert_eq!(loser_source.money, 700);
    assert_eq!(market.mail.len(), 2);
    assert_eq!(market.decisions.len(), 2);
}

#[test]
fn expiry_and_buyout_race_in_either_order_completes_once_and_conserves_value() {
    let listing = PreparedListing {
        request: request(),
        snapshot: item(23).snapshot,
        deposit: 10,
        created_micros: 1_000,
        expires_micros: 7_200_001_000,
    };
    let bid_auction = BidAuction {
        highest_bidder_guid: 9,
        highest_bid: 201,
        buyout: 500,
        ..active_bid_auction()
    };
    let buyout = HoldRequest {
        operation: HoldOperation::Bid,
        operation_id: 909,
        bidder_guid: 8,
        auction_id: 41,
        house: 1,
        offer: 500,
    };

    let mut buyout_source = FakeBidSource::new(700);
    let mut buyout_market = FakeBidMarket {
        auction: Some(bid_auction),
        decisions: Vec::new(),
        mail: Vec::new(),
        notices: Vec::new(),
        expiry_armed: true,
        now_micros: 1_000,
    };
    assert_eq!(
        drive_bid(&mut buyout_source, &mut buyout_market, buyout),
        Ok(accepted_buyout(500, 9, 201))
    );
    let buyout_mail = buyout_market.mail.clone();
    expire_bid_market(&mut buyout_market, listing.clone()).unwrap();
    expire_bid_market(&mut buyout_market, listing.clone()).unwrap();
    assert_eq!(buyout_market.mail, buyout_mail);
    assert_eq!(
        u64::from(buyout_source.money)
            + buyout_market
                .mail
                .iter()
                .map(|mail| u64::from(mail.money))
                .sum::<u64>()
            + 25,
        700 + 201 + 10,
        "the post-buyout purse, refund, proceeds, and cut account for all copper"
    );
    assert_eq!(
        buyout_market
            .mail
            .iter()
            .filter(|mail| !mail.attached_item.is_empty())
            .count(),
        1
    );

    let mut expiry_market = FakeBidMarket {
        auction: Some(bid_auction),
        decisions: Vec::new(),
        mail: Vec::new(),
        notices: Vec::new(),
        expiry_armed: true,
        now_micros: 1_000,
    };
    expire_bid_market(&mut expiry_market, listing.clone()).unwrap();
    expire_bid_market(&mut expiry_market, listing).unwrap();
    let expiry_mail = expiry_market.mail.clone();
    let mut late_buyout_source = FakeBidSource::new(700);
    assert_eq!(
        drive_bid(&mut late_buyout_source, &mut expiry_market, buyout),
        Ok(HoldDecision::ItemNotFound)
    );
    assert_eq!(
        drive_bid(&mut late_buyout_source, &mut expiry_market, buyout),
        Ok(HoldDecision::ItemNotFound)
    );
    assert_eq!(
        late_buyout_source.money, 700,
        "the losing Hold is restored once"
    );
    assert_eq!(expiry_market.mail, expiry_mail);
    assert_eq!(
        u64::from(late_buyout_source.money)
            + expiry_mail
                .iter()
                .map(|mail| u64::from(mail.money))
                .sum::<u64>()
            + 10,
        700 + 201 + 10,
        "the post-expiry purse, proceeds, and cut account for all copper"
    );
    assert_eq!(
        expiry_mail
            .iter()
            .filter(|mail| !mail.attached_item.is_empty())
            .count(),
        1
    );
}

#[test]
fn expiry_preserves_inconsistent_or_overflowing_sale_state_for_repair() {
    let listing = PreparedListing {
        request: request(),
        snapshot: item(23).snapshot,
        deposit: 10,
        created_micros: 1_000,
        expires_micros: 7_200_001_000,
    };
    for (highest_bidder_guid, highest_bid) in [(8, 0), (0, 201)] {
        let mut store = FakeExpiry {
            auction: Some(ActiveAuction {
                id: 41,
                listing: listing.clone(),
                highest_bidder_guid,
                highest_bid,
            }),
            schedule_count: 1,
            mail: Vec::new(),
            notices: Vec::new(),
            returned_items: Vec::new(),
            refunded_copper: 0,
        };
        assert!(expire_active(&mut store, 41).is_err());
        assert!(store.auction.is_some());
        assert_eq!(store.schedule_count, 1);
        assert!(store.mail.is_empty());
    }

    let mut overflowing = listing;
    overflowing.deposit = u32::MAX;
    let mut store = FakeExpiry {
        auction: Some(ActiveAuction {
            id: 41,
            listing: overflowing,
            highest_bidder_guid: 8,
            highest_bid: u32::MAX,
        }),
        schedule_count: 1,
        mail: Vec::new(),
        notices: Vec::new(),
        returned_items: Vec::new(),
        refunded_copper: 0,
    };
    assert!(expire_active(&mut store, 41).is_err());
    assert!(store.auction.is_some());
    assert_eq!(store.schedule_count, 1);
    assert!(store.mail.is_empty());
}

/// Seller 7's listing 41 in house 1 at a 5% cut, with bidder 9's bid of 201. The cut is
/// 201 * 5 / 100 = 10.05, truncated to 10.
fn bid_listing() -> BidAuction {
    BidAuction {
        highest_bidder_guid: 9,
        highest_bid: 201,
        ..active_bid_auction()
    }
}

fn cancel_request(operation_id: u64, cut: u32) -> HoldRequest {
    HoldRequest {
        operation: HoldOperation::Cancel,
        operation_id,
        bidder_guid: 7,
        auction_id: 41,
        house: 1,
        offer: cut,
    }
}

fn cancel_market(auction: BidAuction) -> FakeBidMarket {
    FakeBidMarket {
        auction: Some(auction),
        decisions: Vec::new(),
        mail: Vec::new(),
        notices: Vec::new(),
        expiry_armed: true,
        now_micros: 1_000,
    }
}

const CANCELLED_WITH_BID: HoldDecision = HoldDecision::Cancelled {
    cut: 10,
    displaced_bidder_guid: 9,
    displaced_bid: 201,
};

#[test]
fn realm_cancellation_decision_checks_owner_market_expiry_and_the_held_cut() {
    let listing = Some(bid_listing());
    assert_eq!(
        decide_cancel(listing, cancel_request(950, 10), 1_000),
        CANCELLED_WITH_BID
    );
    assert_eq!(
        decide_cancel(Some(active_bid_auction()), cancel_request(950, 0), 1_000),
        HoldDecision::Cancelled {
            cut: 0,
            displaced_bidder_guid: 0,
            displaced_bid: 0,
        },
        "an unbid listing costs nothing"
    );
    assert_eq!(
        decide_cancel(
            listing,
            HoldRequest {
                house: 3,
                ..cancel_request(950, 10)
            },
            1_000
        ),
        CANCELLED_WITH_BID,
        "Darnassus shares the Alliance market with Stormwind"
    );

    for (cut, why) in [
        (11, "a bid raised the cut"),
        (0, "the listing gained a bid"),
    ] {
        assert_eq!(
            decide_cancel(listing, cancel_request(950, cut), 1_000),
            HoldDecision::Database,
            "{why}"
        );
    }
    for (auction, request, now, why) in [
        (None, cancel_request(950, 10), 1_000, "missing"),
        (listing, cancel_request(950, 10), 2_000, "expired"),
        (
            listing,
            HoldRequest {
                bidder_guid: 8,
                ..cancel_request(950, 10)
            },
            1_000,
            "another player's listing",
        ),
        (
            listing,
            HoldRequest {
                house: 4,
                ..cancel_request(950, 10)
            },
            1_000,
            "the Horde market",
        ),
    ] {
        assert_eq!(
            decide_cancel(auction, request, now),
            HoldDecision::ItemNotFound,
            "{why}"
        );
    }
    for (bidder, bid) in [(9, 0), (0, 201)] {
        let inconsistent = BidAuction {
            highest_bidder_guid: bidder,
            highest_bid: bid,
            ..active_bid_auction()
        };
        assert_eq!(
            decide_cancel(Some(inconsistent), cancel_request(950, 10), 1_000),
            HoldDecision::Database
        );
    }
    let as_bid = HoldRequest {
        operation: HoldOperation::Bid,
        ..cancel_request(950, 10)
    };
    assert_eq!(
        decide_cancel(listing, as_bid, 1_000),
        HoldDecision::Database
    );
    assert_eq!(
        decide_bid(listing, cancel_request(950, 300), 1_000),
        HoldDecision::Database,
        "a Cancellation Hold never buys"
    );
}

#[test]
fn a_cancellation_hold_spends_exactly_the_cut_and_a_refusal_restores_it() {
    let request = cancel_request(951, 10);

    let mut cancelled = FakeBidSource::new(100);
    assert_eq!(fence_bid(&mut cancelled, request), Ok(()));
    assert_eq!(cancelled.money, 90, "the cut is held first");
    for _ in 0..2 {
        assert_eq!(
            finish_bid(&mut cancelled, request, CANCELLED_WITH_BID),
            Ok(CANCELLED_WITH_BID)
        );
        assert_eq!(cancelled.money, 90, "the cut is spent once");
    }

    let mut refused = FakeBidSource::new(100);
    fence_bid(&mut refused, request).unwrap();
    for _ in 0..2 {
        assert_eq!(
            finish_bid(&mut refused, request, HoldDecision::Database),
            Ok(HoldDecision::Database)
        );
        assert_eq!(refused.money, 100, "a refusal restores the cut once");
    }

    let unbid = cancel_request(952, 0);
    let mut free = FakeBidSource::new(0);
    assert_eq!(
        fence_bid(&mut free, unbid),
        Ok(()),
        "a zero cut still Holds"
    );
    assert!(free.hold.is_some());
    let no_cut = HoldDecision::Cancelled {
        cut: 0,
        displaced_bidder_guid: 0,
        displaced_bid: 0,
    };
    assert_eq!(finish_bid(&mut free, unbid, no_cut), Ok(no_cut));
    assert_eq!((free.money, free.deferred_refund), (0, 0));

    let mut overflowed = FakeBidSource::new(100);
    fence_bid(&mut overflowed, request).unwrap();
    overflowed.money = u32::MAX;
    assert_eq!(
        finish_bid(&mut overflowed, request, HoldDecision::ItemNotFound),
        Ok(HoldDecision::ItemNotFound)
    );
    assert_eq!(overflowed.money, u32::MAX);
    assert_eq!(
        overflowed.deferred_refund, 10,
        "a full purse defers the cut"
    );
    assert_eq!(confirm_bid_refund(&mut overflowed, request, 10), Ok(()));
    assert_eq!(overflowed.deferred_refund, 0);

    let mut poor = FakeBidSource::new(9);
    assert_eq!(
        fence_bid(&mut poor, request),
        Err(AuctionRefusal::NotEnoughMoney)
    );
    assert_eq!((poor.money, poor.hold), (9, None));

    let mut crossed = FakeBidSource::new(100);
    fence_bid(&mut crossed, request).unwrap();
    for wrong in [
        HoldDecision::Cancelled {
            cut: 11,
            displaced_bidder_guid: 9,
            displaced_bid: 201,
        },
        accepted_active(10, 4, 9, 201),
    ] {
        assert_eq!(
            finish_bid(&mut crossed, request, wrong),
            Err(AuctionRefusal::Database)
        );
        assert_eq!(crossed.money, 90);
    }
    let mut free_bid = FakeBidSource::new(100);
    assert_eq!(
        fence_bid(
            &mut free_bid,
            HoldRequest {
                operation: HoldOperation::Bid,
                ..unbid
            }
        ),
        Err(AuctionRefusal::Database),
        "only a Cancellation may hold nothing"
    );
}

#[test]
fn a_cancellation_removes_the_listing_and_mails_the_item_and_the_bid_back_once() {
    let item = item(23).snapshot;
    let mut market = cancel_market(bid_listing());
    for _ in 0..2 {
        assert_eq!(
            resolve_bid(&mut market, cancel_request(953, 10)),
            Ok(CANCELLED_WITH_BID)
        );
    }
    assert_eq!(market.auction, None);
    assert!(!market.expiry_armed);
    let blank = AuctionMail {
        recipient_guid: 0,
        house: 1,
        action: AuctionMailAction::Cancelled,
        item_entry: 25,
        random_property_id: 117,
        money: 0,
        attached_item: crate::items::ItemSnapshot::default(),
        counterparty_guid: 0,
        bid: 0,
        buyout: 0,
        deposit: 0,
        cut: 0,
    };
    assert_eq!(
        market.mail,
        vec![
            AuctionMail {
                recipient_guid: 9,
                action: AuctionMailAction::CancelledToBidder,
                money: 201,
                ..blank
            },
            AuctionMail {
                recipient_guid: 7,
                attached_item: item,
                ..blank
            },
        ],
        "the bid goes back to the bidder, the item to the seller, the deposit to nobody"
    );
    assert_eq!(
        market.notices,
        vec![AuctionNoticeDraft {
            recipient_guid: 9,
            kind: 5,
            house: 1,
            auction_id: 41,
            item_entry: 25,
            random_property_id: 117,
            bid: 201,
            out_bid: 0,
            bidder_guid: 9,
        }]
    );
    assert_eq!(market.decisions.len(), 1, "a replay records nothing");

    for crossed in [
        HoldRequest {
            operation: HoldOperation::Bid,
            ..cancel_request(953, 10)
        },
        cancel_request(953, 11),
    ] {
        assert_eq!(
            resolve_bid(&mut market, crossed),
            Err(AuctionRefusal::Database)
        );
    }
    assert_eq!(market.mail.len(), 2);

    let mut unbid = cancel_market(active_bid_auction());
    resolve_bid(&mut unbid, cancel_request(954, 0)).unwrap();
    assert_eq!(
        unbid.mail,
        vec![AuctionMail {
            recipient_guid: 7,
            attached_item: item,
            ..blank
        }]
    );
    assert!(unbid.notices.is_empty(), "nobody was displaced");
}

#[test]
fn a_bid_or_a_settlement_after_the_read_refuses_the_cancellation_and_refunds_the_cut() {
    let mut seller = FakeBidSource::new(100);
    let mut market = cancel_market(bid_listing());
    let cancel = cancel_request(955, 10);
    fence_bid(&mut seller, cancel).unwrap();
    assert_eq!(seller.money, 90);

    let raise = HoldRequest {
        operation: HoldOperation::Bid,
        operation_id: 956,
        bidder_guid: 8,
        auction_id: 41,
        house: 1,
        offer: 300,
    };
    let mut bidder = FakeBidSource::new(1_000);
    assert_eq!(
        drive_bid(&mut bidder, &mut market, raise),
        Ok(accepted_active(300, 4, 9, 201))
    );

    assert_eq!(
        drive_bid(&mut seller, &mut market, cancel),
        Ok(HoldDecision::Database)
    );
    assert_eq!(seller.money, 100, "the seller gets the whole cut back");
    assert_eq!(
        market
            .auction
            .map(|auction| (auction.highest_bidder_guid, auction.highest_bid)),
        Some((8, 300)),
        "the listing stays with the new bid"
    );
    assert!(market
        .mail
        .iter()
        .all(|mail| mail.action == AuctionMailAction::Outbid));

    // A settlement that wins the race removes the listing first.
    let mut settled = cancel_market(bid_listing());
    let mut late_seller = FakeBidSource::new(100);
    let late = cancel_request(957, 10);
    fence_bid(&mut late_seller, late).unwrap();
    settled.auction = None;
    assert_eq!(
        drive_bid(&mut late_seller, &mut settled, late),
        Ok(HoldDecision::ItemNotFound)
    );
    assert_eq!(late_seller.money, 100);
    assert!(settled.mail.is_empty());
}

#[test]
fn local_and_interrupted_sharded_cancellations_have_equivalent_state() {
    for (operation_id, auction, cut) in [(958, bid_listing(), 10), (959, active_bid_auction(), 0)] {
        let request = cancel_request(operation_id, cut);
        let source = FakeBidSource::new(100);
        let market = cancel_market(auction);

        let mut local_source = source;
        let mut local_market = market.clone();
        let expected = drive_bid(&mut local_source, &mut local_market, request).unwrap();
        assert!(matches!(expected, HoldDecision::Cancelled { .. }));
        assert_eq!(local_source.money, 100 - cut);

        for killed_after in 0..=2 {
            let mut sharded_source = source;
            let mut sharded_market = market.clone();
            if killed_after >= 1 {
                fence_bid(&mut sharded_source, request).unwrap();
            }
            if killed_after >= 2 {
                resolve_bid(&mut sharded_market, request).unwrap();
            }
            for _ in 0..2 {
                assert_eq!(
                    drive_bid(&mut sharded_source, &mut sharded_market, request),
                    Ok(expected)
                );
            }
            assert_eq!(sharded_source.money, local_source.money);
            assert_eq!(sharded_source.hold, local_source.hold);
            assert_eq!(sharded_market.auction, local_market.auction);
            assert_eq!(sharded_market.decisions, local_market.decisions);
            assert_eq!(sharded_market.mail, local_market.mail);
            assert_eq!(sharded_market.notices, local_market.notices);
            assert_eq!(sharded_market.expiry_armed, local_market.expiry_armed);
        }
    }
}

#[test]
fn rows_written_before_cancellation_existed_read_as_bids() {
    assert_eq!(HoldOperation::from_code(0), Some(HoldOperation::Bid));
    assert_eq!(HoldOperation::from_code(1), Some(HoldOperation::Cancel));
    assert_eq!(HoldOperation::from_code(2), None);

    let legacy_acceptance = BidDecisionFields {
        outcome: 1,
        revision: 4,
        result_bidder_guid: 9,
        result_bid: 201,
        minimum_increment: 0,
        accepted_price: 0,
    };
    assert_eq!(
        bid_decision_from_fields(legacy_acceptance, 107),
        Some(accepted_active(107, 4, 9, 201)),
        "a bid row from before accepted_price existed still charges its offer"
    );
    let free_cancellation = BidDecisionFields {
        outcome: 7,
        accepted_price: 0,
        ..legacy_acceptance
    };
    assert_eq!(
        bid_decision_from_fields(free_cancellation, 107),
        Some(HoldDecision::Cancelled {
            cut: 0,
            displaced_bidder_guid: 9,
            displaced_bid: 201,
        }),
        "a zero cut is a real cut, never the offer"
    );
}
