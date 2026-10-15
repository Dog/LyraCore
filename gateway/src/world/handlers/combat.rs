use super::super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PetRenameOutcome {
    Renamed,
    NameInvalid,
}

pub(crate) trait CombatStore: Send + Sync {
    fn set_target(&self, actor: Actor, target_guid: u64) -> Result<()>;

    /// PET_ACTION packs flag << 24 | id; flag 0x07 is command and 0x06 is react state.
    fn pet_command(&self, actor: Actor, data: u32, target_guid: u64) -> Result<()>;

    /// Returns NameInvalid for a bad name; other Refusals and Transport Loss stay Err.
    fn pet_rename(&self, actor: Actor, pet_guid: u64, name: &str) -> Result<PetRenameOutcome>;

    fn set_sheathed(&self, actor: Actor, state: u8) -> Result<()>;
}

pub(super) fn ignore_refusal(action: &str, account_id: u64, result: Result<()>) -> Result<()> {
    let Err(error) = result else {
        return Ok(());
    };
    if matches!(
        crate::stdb::classify(&error),
        crate::stdb::DurableFailure::TransportLoss
    ) {
        return Err(error);
    }
    log::debug!("world: {action} ignored (account {account_id}): {error}");
    Ok(())
}

pub(crate) struct Combat;

impl<St: CombatStore + ?Sized> ProtocolFamily<St> for Combat {
    fn handle(
        store: &St,
        conn: &mut ProtocolSession,
        request: ProtocolRequest,
    ) -> Result<ProtocolReply> {
        let msg = request.message()?;
        let actor = conn.self_guid().and_then(Actor::new);

        match msg {
            ClientOpcodeMessage::CMSG_SET_SELECTION(s) => {
                let actor =
                    actor.ok_or_else(|| anyhow!("CMSG_SET_SELECTION before world entry"))?;
                store.set_target(actor, s.target.guid())?
            }
            ClientOpcodeMessage::CMSG_PET_ACTION(p) => {
                if let Some(actor) = actor {
                    let result = store.pet_command(actor, p.data, p.target.guid());
                    ignore_refusal("pet_command", conn.account_id, result)?;
                }
            }
            ClientOpcodeMessage::CMSG_PET_RENAME(r) => {
                if let Some(actor) = actor {
                    match store.pet_rename(actor, r.pet.guid(), &r.name) {
                        Ok(PetRenameOutcome::Renamed) => {}
                        Ok(PetRenameOutcome::NameInvalid) => {
                            return Ok(vec![Outbound::One(
                                ServerOpcodeMessage::SMSG_PET_NAME_INVALID,
                            )]
                            .into());
                        }
                        Err(error) => ignore_refusal("pet_rename", conn.account_id, Err(error))?,
                    }
                }
            }
            ClientOpcodeMessage::CMSG_FORCE_RUN_SPEED_CHANGE_ACK(_) => {}
            ClientOpcodeMessage::CMSG_SETSHEATHED(s) => {
                let state = s.sheathed.as_int();
                if let Some(actor) = actor {
                    let result = store.set_sheathed(actor, state);
                    ignore_refusal(&format!("set_sheathed({state})"), conn.account_id, result)?;
                }
            }
            // vmangos `HandleCancelGrowthAuraOpcode` is empty.
            ClientOpcodeMessage::CMSG_CANCEL_GROWTH_AURA => {}
            // No owner-to-pet read exists; the pet bar arrives with the pet's CREATE.
            ClientOpcodeMessage::CMSG_REQUEST_PET_INFO => {}
            _ => return Err(anyhow!("opcode routed to the wrong Protocol Family")),
        }
        Ok(ProtocolReply::default())
    }
}
