use super::super::*;
use super::show_bank;
use crate::stdb::{classify, DurableFailure};

pub(crate) trait BankStore: Send + Sync {
    fn auto_bank_item(&self, actor: Actor, slot: u8) -> Result<()>;

    fn buy_bank_slot(&self, actor: Actor, banker_guid: u64) -> Result<()>;
}

fn auto_bank<St: BankStore + ?Sized>(
    outbound: &mut Vec<Outbound>,
    store: &St,
    conn: &ProtocolSession,
    slot_index: u8,
    direction: &str,
) -> Result<()> {
    if let Some(actor) = conn.self_guid().and_then(Actor::new) {
        let Err(error) = store.auto_bank_item(actor, slot_index) else {
            return Ok(());
        };
        if matches!(classify(&error), DurableFailure::TransportLoss) {
            return Err(error);
        }
        log::debug!(
            "world: auto_bank_item ({direction}) rejected (account {}): {error}",
            conn.account_id
        );
    }
    {
        outbound.push(Outbound::One(
            ServerOpcodeMessage::SMSG_INVENTORY_CHANGE_FAILURE(Box::new(
                codec::build_inventory_change_failure(),
            )),
        ));
        Ok(())
    }
}

pub(crate) struct Bank;

impl<St: BankStore + NpcStore + ?Sized> ProtocolFamily<St> for Bank {
    fn handle(
        store: &St,
        conn: &mut ProtocolSession,
        request: ProtocolRequest,
    ) -> Result<ProtocolReply> {
        let msg = request.message()?;
        let mut outbound = Vec::new();
        const MAIN_BAG: u8 = 255;
        match msg {
            ClientOpcodeMessage::CMSG_BANKER_ACTIVATE(c) => {
                let banker_guid = c.guid.guid();
                if let WorldState::InWorld(iw) = &conn.state {
                    if store
                        .npc_refuses_interaction(banker_guid, iw.self_guid)
                        .unwrap_or(false)
                    {
                        return Ok(outbound.into());
                    }
                }
                outbound.push(show_bank(banker_guid));
            }
            ClientOpcodeMessage::CMSG_AUTOBANK_ITEM(c) => {
                if c.bag_index == MAIN_BAG {
                    auto_bank(&mut outbound, store, conn, c.slot_index, "deposit")?;
                } else {
                    log::debug!(
                        "world: autobank from sub-bag {} unsupported (account {})",
                        c.bag_index,
                        conn.account_id
                    );
                }
            }
            ClientOpcodeMessage::CMSG_BUY_BANK_SLOT(c) => {
                let banker_guid = c.guid.guid();
                let refusal = match conn
                    .self_guid()
                    .and_then(Actor::new)
                    .map(|actor| store.buy_bank_slot(actor, banker_guid))
                {
                    Some(Ok(())) => None,
                    None => Some(String::new()),
                    Some(Err(error)) => match classify(&error) {
                        DurableFailure::Refusal { reason } => Some(reason.to_string()),
                        DurableFailure::TransportLoss => return Err(error),
                    },
                };
                if let Some(reason) = &refusal {
                    log::debug!(
                        "world: buy_bank_slot rejected (account {}): {reason}",
                        conn.account_id
                    );
                }
                outbound.push(Outbound::One(
                    ServerOpcodeMessage::SMSG_BUY_BANK_SLOT_RESULT(
                        codec::build_buy_bank_slot_reply(refusal.as_deref().map_or(Ok(()), Err)),
                    ),
                ));
            }
            ClientOpcodeMessage::CMSG_AUTOSTORE_BANK_ITEM(c) => {
                if c.bag_index == MAIN_BAG {
                    auto_bank(&mut outbound, store, conn, c.slot_index, "withdraw")?;
                } else {
                    log::debug!(
                        "world: autostore-bank from sub-bag {} unsupported (account {})",
                        c.bag_index,
                        conn.account_id
                    );
                }
            }
            _ => return Err(anyhow!("opcode routed to the wrong Protocol Family")),
        }
        Ok(outbound.into())
    }
}
