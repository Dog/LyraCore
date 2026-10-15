use super::handlers::{Combat, CombatStore, PetRenameOutcome};
use super::*;
use crate::stdb::ReducerCallError;
use std::collections::BTreeMap;
use std::sync::Mutex;
use wow_world_messages::vanilla::CMSG_PET_RENAME;

#[derive(Clone, Debug, PartialEq, Eq)]
struct FakePet {
    guid: u64,
    name: String,
    rename_allowed: bool,
}

#[derive(Default)]
struct CombatFake {
    selected: Mutex<BTreeMap<u64, u64>>,
    sheath: Mutex<BTreeMap<u64, u8>>,
    pets: Mutex<BTreeMap<u64, FakePet>>,
    failure: Mutex<Option<ReducerCallError>>,
}

impl CombatFake {
    fn target_of(&self, character: u64) -> Option<u64> {
        self.selected.lock().unwrap().get(&character).copied()
    }

    fn sheath_of(&self, character: u64) -> Option<u8> {
        self.sheath.lock().unwrap().get(&character).copied()
    }

    fn with_pet(self, owner: u64, guid: u64) -> Self {
        self.pets.lock().unwrap().insert(
            owner,
            FakePet {
                guid,
                name: "Mottled Boar".into(),
                rename_allowed: true,
            },
        );
        self
    }

    fn pet_of(&self, owner: u64) -> Option<FakePet> {
        self.pets.lock().unwrap().get(&owner).cloned()
    }

    fn failing(failure: ReducerCallError) -> Self {
        Self {
            failure: Mutex::new(Some(failure)),
            ..Self::default()
        }
    }

    fn answer(&self) -> Result<()> {
        match self.failure.lock().unwrap().take() {
            Some(failure) => Err(failure.into()),
            None => Ok(()),
        }
    }
}

impl CombatStore for CombatFake {
    fn set_target(&self, actor: Actor, target_guid: u64) -> Result<()> {
        self.answer()?;
        self.selected
            .lock()
            .unwrap()
            .insert(actor.guid(), target_guid);
        Ok(())
    }

    fn pet_command(&self, _actor: Actor, _data: u32, _target_guid: u64) -> Result<()> {
        self.answer()
    }

    fn pet_rename(&self, actor: Actor, pet_guid: u64, name: &str) -> Result<PetRenameOutcome> {
        self.answer()?;
        let mut pets = self.pets.lock().unwrap();
        let Some(pet) = pets
            .get_mut(&actor.guid())
            .filter(|pet| pet.guid == pet_guid && pet.rename_allowed)
        else {
            return Err(ReducerCallError::refused("gw_pet_rename", "pet rename: refused").into());
        };
        if !lyracore_shared::pet::is_valid_pet_name(name) {
            return Ok(PetRenameOutcome::NameInvalid);
        }
        pet.name = name.to_string();
        pet.rename_allowed = false;
        Ok(PetRenameOutcome::Renamed)
    }

    fn set_sheathed(&self, actor: Actor, state: u8) -> Result<()> {
        self.answer()?;
        self.sheath.lock().unwrap().insert(actor.guid(), state);
        Ok(())
    }
}

fn run(store: &CombatFake, msg: impl Into<ClientOpcodeMessage>) {
    let mut session = ProtocolSession::in_world(7, 1);
    let reply = Combat::handle(store, &mut session, ProtocolRequest::Message(msg.into())).unwrap();
    assert!(reply.outbound.is_empty());
}

#[test]
fn set_sheathed_routes_the_clients_z_press_to_the_store() {
    for (sent, expect) in [
        (SheathState::Unarmed, 0u8),
        (SheathState::Melee, 1),
        (SheathState::Ranged, 2),
    ] {
        let store = CombatFake::default();
        run(&store, CMSG_SETSHEATHED { sheathed: sent });
        assert_eq!(
            store.sheath_of(1),
            Some(expect),
            "{sent:?} must reach the Store as byte {expect}"
        );
    }
}

#[test]
fn set_selection_records_the_wire_guid_as_the_target() {
    let store = CombatFake::default();
    run(
        &store,
        CMSG_SET_SELECTION {
            target: Guid::new(321),
        },
    );
    assert_eq!(store.target_of(1), Some(321));
}

fn sheathe() -> CMSG_SETSHEATHED {
    CMSG_SETSHEATHED {
        sheathed: SheathState::Melee,
    }
}

#[test]
fn transport_loss_ends_the_world_session() {
    let store = CombatFake::failing(ReducerCallError::transport_lost("gw_set_sheathed"));
    let mut session = ProtocolSession::in_world(7, 1);
    assert!(Combat::handle(
        &store,
        &mut session,
        ProtocolRequest::Message(sheathe().into())
    )
    .is_err());
}

#[test]
fn refusal_is_ignored_and_the_world_session_continues() {
    let store = CombatFake::failing(ReducerCallError::refused("gw_set_sheathed", "not in world"));
    let mut session = ProtocolSession::in_world(7, 1);
    let reply = Combat::handle(
        &store,
        &mut session,
        ProtocolRequest::Message(sheathe().into()),
    )
    .unwrap();
    assert!(reply.outbound.is_empty());
    assert_eq!(store.sheath_of(1), None);
}

#[test]
fn growth_aura_cancel_and_pet_info_requests_are_ignored_without_a_durable_request() {
    let store = CombatFake::failing(ReducerCallError::transport_lost("unexpected"));
    run(&store, ClientOpcodeMessage::CMSG_CANCEL_GROWTH_AURA);
    run(&store, ClientOpcodeMessage::CMSG_REQUEST_PET_INFO);
    assert!(
        store.answer().is_err(),
        "neither request may reach the Store"
    );
}

#[test]
fn a_pet_cast_is_refused_with_the_silent_pet_cast_failure_and_a_notice() {
    use wow_world_messages::vanilla::{SpellCastTargets, CMSG_PET_CAST_SPELL};
    let request = ClientOpcodeMessage::CMSG_PET_CAST_SPELL(Box::new(CMSG_PET_CAST_SPELL {
        guid: Guid::new(0xF140_0000_0000_0009),
        id: 3110,
        targets: SpellCastTargets::default(),
    }));
    let replies = handlers::unavailable_outbound(&request, &mut Default::default())
        .expect("a pet cast must be answered");
    let [Outbound::Raw { opcode, body }, Outbound::One(ServerOpcodeMessage::SMSG_MESSAGECHAT(notice))] =
        replies.as_slice()
    else {
        panic!("expected the pet cast failure, then the notice");
    };
    // SMSG_PET_CAST_FAILED: spell 3110 (Firebolt), status 2 (failed), reason 0x17 (not reported).
    assert_eq!(*opcode, 0x0138);
    assert_eq!(body, &[0x26, 0x0C, 0x00, 0x00, 0x02, 0x17]);
    assert_eq!(
        notice.message,
        "Pet spell casts are not available on this realm."
    );
}

const PET: u64 = 0xF130_0000_0000_0001;

fn rename(pet: u64, name: &str) -> CMSG_PET_RENAME {
    CMSG_PET_RENAME {
        pet: Guid::new(pet),
        name: name.into(),
    }
}

fn replies(store: &CombatFake, msg: impl Into<ClientOpcodeMessage>) -> Vec<Outbound> {
    let mut session = ProtocolSession::in_world(7, 1);
    Combat::handle(store, &mut session, ProtocolRequest::Message(msg.into()))
        .unwrap()
        .outbound
}

#[test]
fn pet_rename_stores_the_name_once_and_answers_nothing() {
    let store = CombatFake::default().with_pet(1, PET);
    assert!(replies(&store, rename(PET, "Fang")).is_empty());
    let renamed = store.pet_of(1).unwrap();
    assert_eq!(renamed.name, "Fang");
    assert!(!renamed.rename_allowed);

    assert!(replies(&store, rename(PET, "Claw")).is_empty());
    assert_eq!(store.pet_of(1).unwrap().name, "Fang");
}

#[test]
fn a_bad_pet_name_answers_name_invalid_and_keeps_the_rename() {
    let store = CombatFake::default().with_pet(1, PET);
    let out = replies(&store, rename(PET, "Fang 2"));
    assert!(matches!(
        out.as_slice(),
        [Outbound::One(ServerOpcodeMessage::SMSG_PET_NAME_INVALID)]
    ));
    let pet = store.pet_of(1).unwrap();
    assert_eq!(pet.name, "Mottled Boar");
    assert!(pet.rename_allowed);
}

#[test]
fn renaming_another_pet_is_silent_and_changes_nothing() {
    let store = CombatFake::default().with_pet(1, PET).with_pet(2, PET + 1);
    assert!(replies(&store, rename(PET + 1, "Fang")).is_empty());
    assert_eq!(store.pet_of(1).unwrap().name, "Mottled Boar");
    assert_eq!(store.pet_of(2).unwrap().name, "Mottled Boar");
}

#[test]
fn a_pet_rename_transport_loss_ends_the_world_session() {
    let store =
        CombatFake::failing(ReducerCallError::transport_lost("gw_pet_rename")).with_pet(1, PET);
    let mut session = ProtocolSession::in_world(7, 1);
    assert!(Combat::handle(
        &store,
        &mut session,
        ProtocolRequest::Message(rename(PET, "Fang").into())
    )
    .is_err());
}
