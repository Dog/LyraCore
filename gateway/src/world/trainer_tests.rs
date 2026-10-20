use super::handlers::{CharacterStore, Trainer, TrainerBuyOutcome, TrainerStore};
use super::*;
use crate::stdb::ReducerCallError;
use lyracore_shared::constants::armor_proficiency::{
    PLATE_PASSIVE_SPELL_ID, PLATE_TRAINER_SPELL_ID,
};
use lyracore_shared::trainer::TrainerRefusal;

#[derive(Default)]
struct TrainerFake {
    npc_refuses: bool,
    refuses_class: bool,
    spells: Vec<codec::TrainerSpellView>,
    skill_lines: std::collections::HashMap<u32, u32>,
    buy_refusal: Option<TrainerRefusal>,
    buy_error: Option<fn() -> anyhow::Error>,
    talent_error: Option<fn() -> anyhow::Error>,
    superseded: Option<u32>,
    learned_spells: Vec<u32>,
    level_and_class: Option<(u8, u8)>,
    talent_grant: u32,
    talent_pane: (u32, u32, u32),
}

impl TrainerFake {
    fn buying_as(level: u8, class: u8) -> Self {
        Self {
            level_and_class: Some((level, class)),
            ..Default::default()
        }
    }
}

npc_store_refusing_by!(TrainerFake, npc_refuses);

impl CharacterStore for TrainerFake {
    fn player_learned_spells(&self, _player_guid: u64) -> Result<Vec<u32>> {
        Ok(self.learned_spells.clone())
    }

    fn spell_modifiers(&self, _character_guid: u64) -> Vec<(u32, u8, i32, bool)> {
        Vec::new()
    }

    fn characters(&self, _account_id: u64) -> Result<Vec<codec::CharacterView>> {
        unimplemented!("characters")
    }

    fn create_character(
        &self,
        _account_id: u64,
        _name: &str,
        _race: u8,
        _class: u8,
        _gender: u8,
        _appearance: codec::Appearance,
    ) -> Result<codec::CharCreateOutcome> {
        unimplemented!("create_character")
    }

    fn delete_character(
        &self,
        _account_id: u64,
        _character: Actor,
    ) -> Result<codec::CharDeleteOutcome> {
        unimplemented!("delete_character")
    }

    fn character_by_guid(&self, _guid: u64) -> Result<Option<codec::CharacterView>> {
        unimplemented!("character_by_guid")
    }

    fn character_exists_on_any_world_shard(&self, _guid: u64) -> Result<bool> {
        unimplemented!("character_exists_on_any_world_shard")
    }

    fn player_skills(&self, _character_guid: u64) -> Result<Vec<(u32, u16, u16)>> {
        unimplemented!("player_skills")
    }

    fn effective_armor(&self, _guid: u64) -> u32 {
        unimplemented!("effective_armor")
    }

    fn effective_magic_resistances(&self, _guid: u64) -> [u32; 6] {
        unimplemented!("effective_magic_resistances")
    }

    fn player_reputations(&self, _player_guid: u64) -> Result<Vec<(i32, i32, bool)>> {
        unimplemented!("player_reputations")
    }

    fn player_actions(&self, _player_guid: u64) -> Result<Vec<(u8, u32, u8)>> {
        unimplemented!("player_actions")
    }

    fn set_stand_state(&self, _actor: Actor, _state: u8) -> Result<()> {
        unimplemented!("set_stand_state")
    }

    fn stand_state(&self, _guid: u64) -> u8 {
        unimplemented!("stand_state")
    }
}

impl TrainerStore for TrainerFake {
    fn set_watched_faction(
        &self,
        _actor: Actor,
        _reputation_index: i32,
    ) -> Result<InteractionOutcome> {
        unimplemented!("set_watched_faction")
    }

    fn trainer_serves(&self, _player_guid: u64, _trainer_guid: u64) -> Result<bool> {
        Ok(!self.refuses_class)
    }

    fn trainer_list(
        &self,
        _player_guid: u64,
        _trainer_guid: u64,
    ) -> Result<Vec<codec::TrainerSpellView>> {
        Ok(self.spells.clone())
    }

    fn buy_trainer_spell(
        &self,
        _actor: Actor,
        _trainer_guid: u64,
        _spell_id: u32,
    ) -> Result<TrainerBuyOutcome> {
        if let Some(refusal) = self.buy_refusal {
            return Ok(refusal.into());
        }
        match self.buy_error {
            Some(error) => Err(error()),
            None => Ok(TrainerBuyOutcome::Learned),
        }
    }

    fn trainer_offer_skill_line(&self, _trainer_guid: u64, spell_id: u32) -> u32 {
        self.skill_lines.get(&spell_id).copied().unwrap_or(0)
    }

    fn talent_grant_spell(&self, _talent_id: u32) -> u32 {
        self.talent_grant
    }

    fn set_action_button(
        &self,
        _actor: Actor,
        _button: u8,
        _action: u32,
        _action_type: u8,
    ) -> Result<()> {
        unimplemented!("set_action_button")
    }

    fn set_faction_at_war(
        &self,
        _actor: Actor,
        _reputation_index: u32,
        _at_war: bool,
    ) -> Result<InteractionOutcome> {
        unimplemented!("set_faction_at_war")
    }

    fn talent_pane_sync(&self, _character_guid: u64, _talent_id: u32) -> (u32, u32, u32) {
        self.talent_pane
    }

    fn talent_points_spent(&self, _character_guid: u64) -> u32 {
        unimplemented!("talent_points_spent")
    }

    fn learn_talent(&self, _actor: Actor, _talent_id: u32) -> Result<()> {
        match self.talent_error {
            Some(error) => Err(error()),
            None => Ok(()),
        }
    }

    fn talent_reset_cost(&self, _character_guid: u64) -> Option<u32> {
        unimplemented!("talent_reset_cost")
    }

    fn reset_talents(&self, _actor: Actor, _trainer_guid: u64) -> Result<InteractionOutcome> {
        unimplemented!("reset_talents")
    }

    fn resolve_learn_target(&self, spell_id: u32) -> u32 {
        spell_id
    }

    fn superseded_old_rank(&self, _new_spell: u32, _player_guid: u64) -> Option<u32> {
        self.superseded
    }

    fn character_presence(&self, _guid: u64) -> Result<Option<(bool, u8, u8, u32)>> {
        Ok(self
            .level_and_class
            .map(|(level, class)| (true, level, class, 0)))
    }
}

fn drive(
    store: &TrainerFake,
    msg: impl Into<ClientOpcodeMessage>,
) -> Result<Vec<ServerOpcodeMessage>> {
    let mut session = ProtocolSession::in_world(7, 1);
    let reply = Trainer::handle(store, &mut session, ProtocolRequest::Message(msg.into()))?;
    Ok(outbound_messages(reply.outbound))
}

fn run(store: &TrainerFake, msg: impl Into<ClientOpcodeMessage>) -> Vec<ServerOpcodeMessage> {
    drive(store, msg).unwrap()
}

fn kinds(sent: &[ServerOpcodeMessage]) -> String {
    sent.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn buy(id: u32) -> CMSG_TRAINER_BUY_SPELL {
    CMSG_TRAINER_BUY_SPELL {
        guid: Guid::new(70),
        id,
    }
}

fn pick_talent() -> CMSG_LEARN_TALENT {
    CMSG_LEARN_TALENT {
        talent: Talent::BurningSoul,
        requested_rank: 0,
    }
}

fn one_spell() -> Vec<codec::TrainerSpellView> {
    vec![codec::TrainerSpellView {
        spell_id: 100,
        cost: 10,
        required_level: 1,
        player_level: 1,
        known: false,
        learn_skill_line: 0,
    }]
}

#[test]
fn trainer_buy_success_replies_succeeded_then_pushes_the_learned_spell() {
    let sent = run(&TrainerFake::default(), buy(1234));
    let [ServerOpcodeMessage::SMSG_TRAINER_BUY_SUCCEEDED(bought), ServerOpcodeMessage::SMSG_LEARNED_SPELL(learned)] =
        sent.as_slice()
    else {
        panic!(
            "expected a buy confirmation then the learned spell, got [{}]",
            kinds(&sent)
        );
    };
    assert_eq!(bought.guid.guid(), 70);
    assert_eq!(bought.id, 1234);
    assert_eq!(learned.id, 1234);
}

#[test]
fn a_riding_buy_confirms_without_echoing_the_offering_as_a_learned_spell() {
    let store = TrainerFake {
        skill_lines: [(50132, lyracore_shared::trainer::RIDING_SKILL_LINE)].into(),
        ..Default::default()
    };
    let sent = run(&store, buy(50132));
    assert!(
        matches!(sent.as_slice(), [ServerOpcodeMessage::SMSG_TRAINER_BUY_SUCCEEDED(m)] if m.id == 50132),
        "a riding buy confirms and sends nothing more, got [{}]",
        kinds(&sent)
    );
}

#[test]
fn a_proficiency_buy_pushes_the_refreshed_armor_mask_after_the_learned_spell() {
    let store = TrainerFake {
        learned_spells: vec![PLATE_PASSIVE_SPELL_ID],
        ..TrainerFake::buying_as(40, 1)
    };
    let sent = run(&store, buy(PLATE_TRAINER_SPELL_ID));
    let [ServerOpcodeMessage::SMSG_TRAINER_BUY_SUCCEEDED(bought), ServerOpcodeMessage::SMSG_LEARNED_SPELL(learned), ServerOpcodeMessage::SMSG_SET_PROFICIENCY(mask)] =
        sent.as_slice()
    else {
        panic!(
            "expected confirmation, learned spell, proficiency mask, got [{}]",
            kinds(&sent)
        );
    };
    assert_eq!(bought.id, 7109);
    assert_eq!(learned.id, 7109);
    assert_eq!(
        mask.class.as_int(),
        4,
        "a proficiency buy refreshes ARMOR only"
    );
    assert!(
        mask.item_sub_class_mask & (1 << lyracore_shared::item::armor_subclass::PLATE) != 0,
        "the trained plate bit must be set: {:#x}",
        mask.item_sub_class_mask
    );
}

#[test]
fn a_weapon_proficiency_buy_pushes_the_refreshed_weapon_mask() {
    let store = TrainerFake {
        learned_spells: vec![196, 198, 201, 202],
        ..TrainerFake::buying_as(10, 1)
    };
    let sent = run(&store, buy(202));
    let [ServerOpcodeMessage::SMSG_TRAINER_BUY_SUCCEEDED(_), ServerOpcodeMessage::SMSG_LEARNED_SPELL(learned), ServerOpcodeMessage::SMSG_SET_PROFICIENCY(mask)] =
        sent.as_slice()
    else {
        panic!(
            "expected confirmation, learned spell, proficiency mask, got [{}]",
            kinds(&sent)
        );
    };
    assert_eq!(learned.id, 202);
    assert_eq!(mask.class.as_int(), 2, "a weapon buy refreshes WEAPON only");
    assert!(
        mask.item_sub_class_mask & (1 << lyracore_shared::item::weapon_subclass::SWORD_2H) != 0,
        "the trained two-handed sword bit must be set: {:#x}",
        mask.item_sub_class_mask
    );
}

#[test]
fn an_ordinary_trainer_buy_pushes_no_proficiency_mask() {
    let sent = run(&TrainerFake::buying_as(40, 1), buy(1234));
    assert!(
        matches!(
            sent.as_slice(),
            [
                ServerOpcodeMessage::SMSG_TRAINER_BUY_SUCCEEDED(_),
                ServerOpcodeMessage::SMSG_LEARNED_SPELL(learned)
            ] if learned.id == 1234
        ),
        "expected only the confirmation and the learned spell, got [{}]",
        kinds(&sent)
    );
}

#[test]
fn trainer_buy_rank_upgrade_supersedes_the_previous_rank_spell() {
    let store = TrainerFake {
        superseded: Some(1233),
        ..Default::default()
    };
    let sent = run(&store, buy(1234));
    let [ServerOpcodeMessage::SMSG_TRAINER_BUY_SUCCEEDED(bought), ServerOpcodeMessage::SMSG_SUPERCEDED_SPELL(replaced)] =
        sent.as_slice()
    else {
        panic!(
            "expected a buy confirmation then a superseded spell, got [{}]",
            kinds(&sent)
        );
    };
    assert_eq!(bought.guid.guid(), 70);
    assert_eq!(bought.id, 1234);
    assert_eq!(
        replaced.new_spell_id, 1233,
        "first wire slot carries the OLD rank"
    );
    assert_eq!(
        replaced.old_spell_id, 1234,
        "second wire slot carries the NEW rank"
    );
}

#[test]
fn every_module_refusal_has_one_client_failure_reason() {
    let expected = |refusal| match refusal {
        TrainerRefusal::NotEnoughMoney => TrainingFailureReason::NotEnoughMoney,
        TrainerRefusal::LevelTooLow | TrainerRefusal::PreviousRankMissing => {
            TrainingFailureReason::NotEnoughSkill
        }
        TrainerRefusal::Unavailable | TrainerRefusal::NotOffered | TrainerRefusal::AlreadyKnown => {
            TrainingFailureReason::Unavailable
        }
    };
    for refusal in TrainerRefusal::ALL {
        let store = TrainerFake {
            buy_refusal: Some(refusal),
            ..Default::default()
        };
        let sent = run(&store, buy(1234));
        let [ServerOpcodeMessage::SMSG_TRAINER_BUY_FAILED(failed)] = sent.as_slice() else {
            panic!("{refusal:?}: expected one failure, got [{}]", kinds(&sent));
        };
        assert_eq!(failed.error, expected(refusal), "{refusal:?}");
        assert_eq!(failed.id, 1234);
    }
}

#[test]
fn a_trainer_transport_loss_is_not_answered_as_a_refusal() {
    let store = TrainerFake {
        buy_error: Some(|| ReducerCallError::transport_lost("gw_trainer_buy").into()),
        ..Default::default()
    };
    drive(&store, buy(1234))
        .expect_err("a lost trainer Durable Request must end the World Session");
}

#[test]
fn a_refused_talent_pick_keeps_the_session_and_a_transport_loss_ends_it() {
    let refused = TrainerFake {
        talent_error: Some(|| {
            ReducerCallError::refused("gw_learn_talent", "no talent points left").into()
        }),
        ..Default::default()
    };
    let sent = drive(&refused, pick_talent()).unwrap();
    assert!(sent.is_empty(), "got [{}]", kinds(&sent));

    let lost = TrainerFake {
        talent_error: Some(|| ReducerCallError::transport_lost("gw_learn_talent").into()),
        ..Default::default()
    };
    drive(&lost, pick_talent()).expect_err("a lost talent pick must end the World Session");
}

#[test]
fn learn_talent_with_a_grant_spell_pushes_learned_spell() {
    let store = TrainerFake {
        talent_grant: 2098,
        ..Default::default()
    };
    let sent = run(&store, pick_talent());
    assert!(
        matches!(
            sent.as_slice(),
            [
                ServerOpcodeMessage::SMSG_LEARNED_SPELL(granted),
                ServerOpcodeMessage::SMSG_UPDATE_OBJECT(_)
            ] if granted.id == 2098
        ),
        "expected the granted spell then the CHARACTER_POINTS1 push, got [{}]",
        kinds(&sent)
    );
}

#[test]
fn learn_talent_passive_pushes_rank_spell_and_points() {
    let store = TrainerFake {
        talent_pane: (7777, 0, 2),
        ..Default::default()
    };
    let sent = run(&store, pick_talent());
    assert!(
        matches!(
            sent.as_slice(),
            [
                ServerOpcodeMessage::SMSG_LEARNED_SPELL(rank),
                ServerOpcodeMessage::SMSG_UPDATE_OBJECT(_)
            ] if rank.id == 7777
        ),
        "expected the rank spell then the CHARACTER_POINTS1 push, got [{}]",
        kinds(&sent)
    );
}

#[test]
fn learn_talent_rank_upgrade_supersedes_the_previous_rank_spell() {
    let store = TrainerFake {
        talent_pane: (7778, 7777, 1),
        ..Default::default()
    };
    let sent = run(&store, pick_talent());
    let [ServerOpcodeMessage::SMSG_SUPERCEDED_SPELL(replaced), ServerOpcodeMessage::SMSG_UPDATE_OBJECT(_)] =
        sent.as_slice()
    else {
        panic!(
            "expected a superseded spell then the points push, got [{}]",
            kinds(&sent)
        );
    };
    assert_eq!(
        replaced.new_spell_id, 7777,
        "first wire slot carries the OLD rank"
    );
    assert_eq!(
        replaced.old_spell_id, 7778,
        "second wire slot carries the NEW rank"
    );
}

#[test]
fn trainer_list_replies_smsg_trainer_list_with_the_fixture_spells() {
    let store = TrainerFake {
        spells: one_spell(),
        ..Default::default()
    };
    let sent = run(
        &store,
        CMSG_TRAINER_LIST {
            guid: Guid::new(70),
        },
    );
    let [ServerOpcodeMessage::SMSG_TRAINER_LIST(list)] = sent.as_slice() else {
        panic!("expected one trainer list, got [{}]", kinds(&sent));
    };
    assert_eq!(list.guid, Guid::new(70));
    assert_eq!(list.spells.len(), 1, "the fixture's one spell row");
    assert_eq!(list.spells[0].spell, 100);
    assert_eq!(list.trainer_type, 0);
}

#[test]
fn direct_profession_training_request_sends_the_profession_list_type() {
    let mut spells = one_spell();
    spells[0].spell_id = 4036;
    spells[0].learn_skill_line = 202;
    let store = TrainerFake {
        spells,
        ..Default::default()
    };
    let sent = run(
        &store,
        CMSG_TRAINER_LIST {
            guid: Guid::new(70),
        },
    );
    let [ServerOpcodeMessage::SMSG_TRAINER_LIST(list)] = sent.as_slice() else {
        panic!("expected one trainer list, got [{}]", kinds(&sent));
    };
    assert_eq!(list.trainer_type, 2);
    assert_eq!(list.spells[0].spell, 4036);
}

#[test]
fn trainer_list_is_silently_dropped_for_a_player_the_trainer_does_not_serve() {
    let store = TrainerFake {
        refuses_class: true,
        spells: one_spell(),
        ..Default::default()
    };
    let sent = run(
        &store,
        CMSG_TRAINER_LIST {
            guid: Guid::new(70),
        },
    );
    assert!(
        sent.is_empty(),
        "a trainer that does not serve this class sends no window, got [{}]",
        kinds(&sent)
    );
}
