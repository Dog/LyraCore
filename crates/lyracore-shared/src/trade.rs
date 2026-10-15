// Durable event kinds differ from vanilla TradeStatus discriminants.
pub mod event_kind {
    pub const BEGIN_TRADE: u8 = 0;
    pub const OPEN_WINDOW: u8 = 1;
    pub const TRADE_CANCELED: u8 = 2;
    pub const BUSY: u8 = 3;
    pub const NO_TARGET: u8 = 4;
    pub const TARGET_TO_FAR: u8 = 5;
    pub const WRONG_FACTION: u8 = 6;
    pub const YOU_DEAD: u8 = 7;
    pub const TARGET_DEAD: u8 = 8;
    pub const OFFER_SELF: u8 = 9;
    pub const OFFER_PARTNER: u8 = 10;
    pub const IGNORE_YOU: u8 = 11;
    pub const TRADE_ACCEPT: u8 = 12;
    pub const BACK_TO_TRADE: u8 = 13;
    pub const TRADE_COMPLETE: u8 = 14;
    pub const INV_FULL_SELF: u8 = 15;
    pub const INV_FULL_PARTNER: u8 = 16;
    pub const GOLD_FAIL_SELF: u8 = 17;
    pub const GOLD_FAIL_PARTNER: u8 = 18;
}

// Resolve item and template fields in the offer transaction to avoid relay read races.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfferSlot {
    // Slots 0..=5 trade; slot 6 is shown but never committed.
    pub trade_slot: u8,
    pub entry: u32,
    pub display_id: u32,
    pub stack_count: u32,
    pub enchantment: u32,
    pub durability: u32,
    pub max_durability: u32,
    pub random_property_id: u32,
}

pub fn encode_offer(gold: u32, slots: &[OfferSlot]) -> String {
    let body: Vec<String> = slots
        .iter()
        .map(|s| {
            format!(
                "{},{},{},{},{},{},{},{}",
                s.trade_slot,
                s.entry,
                s.display_id,
                s.stack_count,
                s.enchantment,
                s.durability,
                s.max_durability,
                s.random_property_id
            )
        })
        .collect();
    format!("{gold}|{}", body.join(";"))
}

pub fn decode_offer(payload: &str) -> Option<(u32, Vec<OfferSlot>)> {
    let (gold, body) = payload.split_once('|')?;
    let gold: u32 = gold.parse().ok()?;
    let mut slots = Vec::new();
    for part in body.split(';').filter(|p| !p.is_empty()) {
        let f: Vec<&str> = part.split(',').collect();
        if f.len() != 7 && f.len() != 8 {
            return None;
        }
        slots.push(OfferSlot {
            trade_slot: f[0].parse().ok()?,
            entry: f[1].parse().ok()?,
            display_id: f[2].parse().ok()?,
            stack_count: f[3].parse().ok()?,
            enchantment: f[4].parse().ok()?,
            durability: f[5].parse().ok()?,
            max_durability: f[6].parse().ok()?,
            random_property_id: f.get(7).map(|v| v.parse()).transpose().ok()?.unwrap_or(0),
        });
    }
    Some((gold, slots))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offer_payload_round_trips_and_fails_closed_on_malformed_input() {
        let slots = vec![
            OfferSlot {
                trade_slot: 0,
                entry: 2589,
                display_id: 7026,
                stack_count: 20,
                enchantment: 0,
                durability: 0,
                max_durability: 0,
                random_property_id: 117,
            },
            OfferSlot {
                trade_slot: 6,
                entry: 6948,
                display_id: 6418,
                stack_count: 1,
                enchantment: 2564,
                durability: 34,
                max_durability: 40,
                random_property_id: 0,
            },
        ];
        let payload = encode_offer(1_2345, &slots);
        assert_eq!(decode_offer(&payload), Some((1_2345, slots)));

        assert_eq!(decode_offer(&encode_offer(0, &[])), Some((0, Vec::new())));

        assert_eq!(decode_offer(""), None, "no delimiter");
        assert_eq!(decode_offer("abc|"), None, "non-numeric gold");
        assert_eq!(decode_offer("5|1,2,3"), None, "truncated slot");
        assert_eq!(
            decode_offer("5|1,2,3,4,5,6,x"),
            None,
            "one bad field drops the whole payload"
        );
    }
}
