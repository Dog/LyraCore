use super::super::*;
use super::combat::ignore_refusal;

pub(crate) trait TradeStore: Send + Sync {
    fn initiate_trade(&self, actor: Actor, target_guid: u64) -> Result<()>;

    fn begin_trade(&self, actor: Actor) -> Result<()>;

    fn cancel_trade(&self, actor: Actor) -> Result<()>;

    fn set_trade_item(&self, actor: Actor, trade_slot: u8, inv_slot: u8) -> Result<()>;

    fn clear_trade_item(&self, actor: Actor, trade_slot: u8) -> Result<()>;

    fn set_trade_gold(&self, actor: Actor, copper: u32) -> Result<()>;

    fn accept_trade(&self, actor: Actor) -> Result<()>;

    fn unaccept_trade(&self, actor: Actor) -> Result<()>;

    fn busy_trade(&self, actor: Actor) -> Result<()>;

    fn ignore_trade(&self, actor: Actor) -> Result<()>;
}

pub(crate) struct Trade;

impl<St: TradeStore + ?Sized> ProtocolFamily<St> for Trade {
    fn handle(
        store: &St,
        conn: &mut ProtocolSession,
        request: ProtocolRequest,
    ) -> Result<ProtocolReply> {
        let msg = request.message()?;
        let account_id = conn.account_id;
        let actor = conn.self_guid().and_then(Actor::new);
        let result = match (msg, actor) {
            (ClientOpcodeMessage::CMSG_INITIATE_TRADE(c), Some(actor)) => {
                ("initiate_trade", store.initiate_trade(actor, c.guid.guid()))
            }
            (ClientOpcodeMessage::CMSG_BEGIN_TRADE, Some(actor)) => {
                ("begin_trade", store.begin_trade(actor))
            }
            (ClientOpcodeMessage::CMSG_CANCEL_TRADE, Some(actor)) => {
                ("cancel_trade", store.cancel_trade(actor))
            }
            (ClientOpcodeMessage::CMSG_SET_TRADE_ITEM(c), Some(actor)) => {
                const MAIN_BAG: u8 = 255;
                if c.bag != MAIN_BAG {
                    log::debug!(
                        "world: set_trade_item from sub-bag {} ignored (account {account_id})",
                        c.bag
                    );
                    return Ok(ProtocolReply::default());
                }
                (
                    "set_trade_item",
                    store.set_trade_item(actor, c.trade_slot, c.slot),
                )
            }
            (ClientOpcodeMessage::CMSG_CLEAR_TRADE_ITEM(c), Some(actor)) => (
                "clear_trade_item",
                store.clear_trade_item(actor, c.trade_slot),
            ),
            (ClientOpcodeMessage::CMSG_SET_TRADE_GOLD(c), Some(actor)) => (
                "set_trade_gold",
                store.set_trade_gold(actor, c.gold.as_int()),
            ),
            // CMSG_ACCEPT_TRADE's unknown1 is padding; vanilla ignores it.
            (ClientOpcodeMessage::CMSG_ACCEPT_TRADE(_), Some(actor)) => {
                ("accept_trade", store.accept_trade(actor))
            }
            (ClientOpcodeMessage::CMSG_UNACCEPT_TRADE, Some(actor)) => {
                ("unaccept_trade", store.unaccept_trade(actor))
            }
            (ClientOpcodeMessage::CMSG_BUSY_TRADE, Some(actor)) => {
                ("busy_trade", store.busy_trade(actor))
            }
            (ClientOpcodeMessage::CMSG_IGNORE_TRADE, Some(actor)) => {
                ("ignore_trade", store.ignore_trade(actor))
            }
            (
                ClientOpcodeMessage::CMSG_INITIATE_TRADE(_)
                | ClientOpcodeMessage::CMSG_BEGIN_TRADE
                | ClientOpcodeMessage::CMSG_CANCEL_TRADE
                | ClientOpcodeMessage::CMSG_SET_TRADE_ITEM(_)
                | ClientOpcodeMessage::CMSG_CLEAR_TRADE_ITEM(_)
                | ClientOpcodeMessage::CMSG_SET_TRADE_GOLD(_)
                | ClientOpcodeMessage::CMSG_ACCEPT_TRADE(_)
                | ClientOpcodeMessage::CMSG_UNACCEPT_TRADE
                | ClientOpcodeMessage::CMSG_BUSY_TRADE
                | ClientOpcodeMessage::CMSG_IGNORE_TRADE,
                None,
            ) => return Ok(ProtocolReply::default()),
            (_, _) => return Err(anyhow!("opcode routed to the wrong Protocol Family")),
        };
        let (action, result) = result;
        ignore_refusal(action, account_id, result)?;
        Ok(ProtocolReply::default())
    }
}
