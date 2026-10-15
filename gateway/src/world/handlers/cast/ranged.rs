use super::*;

pub(super) fn activate<St: CastStore + ?Sized>(
    store: &St,
    session: &mut ProtocolSession,
    c: &CMSG_CAST_SPELL,
) -> Result<ProtocolReply> {
    let spell = c.spell;
    let target = unit_target(c);
    log::info!(
        "world[autoshot]: activate spell={spell} target={target} already_repeating={} (account {})",
        matches!(&session.state, WorldState::InWorld(world) if world.ranged_repeat),
        session.account_id
    );

    let Some(actor) = session.actor() else {
        return refuse(store, session, spell, NO_ACTOR);
    };
    if let Err(e) = store.start_ranged_attack(actor, target, spell) {
        return refuse(store, session, spell, &refusal_reason(e)?);
    }

    // Auto-repeat activation sends START alone; CAST_RESULT or GO would clear its repeat slot.
    let caster = actor.guid();
    let outbound = vec![Outbound::One(ServerOpcodeMessage::SMSG_SPELL_START(
        Box::new(codec::build_spell_start(
            caster,
            spell,
            0,
            target,
            ammo_display(store, caster),
        )),
    ))];
    Ok({
        if let WorldState::InWorld(world) = &mut session.state {
            world.ranged_repeat = true;
        }
        ProtocolReply::from(outbound)
    })
}

fn refuse<St: CastStore + ?Sized>(
    store: &St,
    session: &mut ProtocolSession,
    spell: u32,
    reason: &str,
) -> Result<ProtocolReply> {
    log::info!(
        "world[autoshot]: start_ranged_attack refused spell={spell} (account {}): {reason}",
        session.account_id
    );
    let outbound = vec![Outbound::Raw {
        opcode: OP_CAST_RESULT,
        body: codec::build_cast_result_failed(spell, codec::cast_failure_reason_for(reason)),
    }];
    if !matches!(&session.state, WorldState::InWorld(world) if world.ranged_repeat) {
        return Ok(outbound.into());
    }
    stop_loop(store, session, "reject-teardown")?;
    Ok({
        if let WorldState::InWorld(world) = &mut session.state {
            world.ranged_repeat = false;
        }
        ProtocolReply::from(outbound)
    })
}

pub(super) fn cancel<St: CastStore + ?Sized>(
    store: &St,
    session: &mut ProtocolSession,
) -> Result<ProtocolReply> {
    log::info!(
        "world[autoshot]: cancel auto-repeat active={} (account {})",
        matches!(&session.state, WorldState::InWorld(world) if world.ranged_repeat),
        session.account_id
    );
    // An unconditional stop could erase a melee engagement that just replaced the ranged loop.
    if matches!(&session.state, WorldState::InWorld(world) if world.ranged_repeat) {
        stop_loop(store, session, "cancel_auto_repeat")?;
    }
    Ok({
        if let WorldState::InWorld(world) = &mut session.state {
            world.ranged_repeat = false;
        }
        ProtocolReply::from(Vec::new())
    })
}

fn stop_loop<St: CastStore + ?Sized>(
    store: &St,
    session: &mut ProtocolSession,
    context: &str,
) -> Result<()> {
    let Some(actor) = session.actor() else {
        return Ok(());
    };
    if let Err(e) = store.stop_attack(actor) {
        let reason = refusal_reason(e)?;
        log::debug!(
            "world: {context} stop_attack ignored (account {}): {reason}",
            session.account_id
        );
    }
    Ok(())
}

fn ammo_display<St: CastStore + ?Sized>(store: &St, self_guid: u64) -> Option<(u32, u32)> {
    let items = store.player_items(self_guid).ok()?;
    let launcher = items
        .iter()
        .find(|i| i.slot == 17)
        .and_then(|i| store.item_template(i.entry).ok().flatten())?;
    if launcher.class != 2 || !matches!(launcher.subclass, 2 | 3 | 18) {
        return None;
    }
    lyracore_shared::item::loaded_ammo(items.iter(), |item| {
        (
            item.slot,
            item.stack_count,
            store
                .item_template(item.entry)
                .ok()
                .flatten()
                .map(|tmpl| tmpl.class),
        )
    })
    .and_then(|item| store.item_template(item.entry).ok().flatten())
    .map(|tmpl| (tmpl.display_id, 24))
}

#[cfg(test)]
mod tests {
    use super::super::tests::*;
    use super::*;
    use codec::{ItemInstanceView, ItemTemplateView};

    const AUTO_SHOT: u32 = 75;

    fn owned(slot: u8, entry: u32, stack_count: u32) -> ItemInstanceView {
        ItemInstanceView {
            guid: u64::from(entry),
            entry,
            owner_guid: CASTER,
            slot,
            stack_count,
            ..Default::default()
        }
    }

    fn launcher(entry: u32, subclass: u8) -> ItemTemplateView {
        ItemTemplateView {
            entry,
            class: 2,
            subclass,
            ..Default::default()
        }
    }

    fn projectile(entry: u32, display_id: u32) -> ItemTemplateView {
        ItemTemplateView {
            entry,
            class: 6,
            display_id,
            ..Default::default()
        }
    }

    fn ranged_store() -> InMemoryCasts {
        InMemoryCasts {
            ranged_auto_repeat: vec![AUTO_SHOT],
            ..InMemoryCasts::instant()
        }
    }

    fn repeating() -> ProtocolSession {
        let mut session = session();
        if let WorldState::InWorld(world) = &mut session.state {
            world.ranged_repeat = true;
        }
        session
    }

    fn spell_start(outbound: &[Outbound]) -> &wow_world_messages::vanilla::SMSG_SPELL_START {
        outbound
            .iter()
            .find_map(|out| match out {
                Outbound::One(ServerOpcodeMessage::SMSG_SPELL_START(start)) => Some(&**start),
                _ => None,
            })
            .expect("the batch has no SMSG_SPELL_START")
    }

    fn ammo_block(outbound: &[Outbound]) -> Option<(u32, u32)> {
        spell_start(outbound)
            .flags
            .get_ammo()
            .map(|a| (a.ammo_display_id, a.ammo_inventory_type))
    }

    #[test]
    fn activation_arms_the_durable_loop_and_acknowledges_with_a_zero_timer_start() {
        let store = ranged_store();

        let (repeating, outbound) =
            handled(run_cast(&store, session(), cast(AUTO_SHOT, unit_targets(88))).unwrap());

        assert_eq!(
            store.ranged_attacks.lock().unwrap().as_slice(),
            &[(CASTER, 88, AUTO_SHOT)],
            "the cast's unit target arms the loop, with no selection fallback"
        );
        assert!(repeating);
        assert_eq!(
            sequence(&outbound),
            ["START"],
            "no CAST_RESULT(OK) and no GO: the activation is not a completed shot"
        );
        let start = spell_start(&outbound);
        assert_eq!(
            start.timer, 0,
            "the wind-up is an attack timer, not a cast bar"
        );
        assert_eq!(
            start
                .targets
                .target_flags
                .get_unit()
                .map(|u| u.unit_target.guid()),
            Some(88)
        );
        assert!(store.casts.lock().unwrap().is_empty());
        assert!(store.stop_attacks.lock().unwrap().is_empty());
    }

    #[test]
    fn fresh_activation_failure_sends_only_the_failure_and_stops_nothing() {
        let store = InMemoryCasts {
            ranged_error: Some("no ranged weapon equipped".into()),
            ..ranged_store()
        };

        let (repeating, outbound) =
            handled(run_cast(&store, session(), cast(AUTO_SHOT, unit_targets(88))).unwrap());

        assert_eq!(sequence(&outbound), ["CAST_RESULT(FAILED 0x17)"]);
        assert!(
            !repeating,
            "a refused activation must not arm ranged-repeat state"
        );
        assert!(
            store.stop_attacks.lock().unwrap().is_empty(),
            "there is no older loop to tear down"
        );
    }

    #[test]
    fn retarget_failure_over_an_active_loop_clears_the_state_and_stops_the_old_loop() {
        let store = InMemoryCasts {
            ranged_error: Some("target is out of range".into()),
            ..ranged_store()
        };

        let (repeating, outbound) =
            handled(run_cast(&store, repeating(), cast(AUTO_SHOT, unit_targets(99))).unwrap());

        assert_eq!(sequence(&outbound), ["CAST_RESULT(FAILED 0x59)"]);
        assert!(!repeating);
        assert_eq!(
            store.stop_attacks.lock().unwrap().as_slice(),
            &[CASTER],
            "the client dropped its toggle, so the still-firing old loop must go too"
        );
    }

    #[test]
    fn a_dead_reducer_transport_during_activation_is_session_fatal() {
        let store = InMemoryCasts {
            transport_lost: true,
            ..ranged_store()
        };

        assert!(run_cast(&store, session(), cast(AUTO_SHOT, unit_targets(88))).is_err());
    }

    #[test]
    fn a_dead_reducer_transport_while_stopping_the_loop_is_session_fatal() {
        let store = InMemoryCasts {
            transport_lost: true,
            ..ranged_store()
        };

        assert!(run_cast(
            &store,
            repeating(),
            ClientOpcodeMessage::CMSG_CANCEL_AUTO_REPEAT_SPELL
        )
        .is_err());
    }

    #[test]
    fn cancelling_an_active_loop_clears_the_state_and_stops_it_with_no_inline_ack() {
        let store = ranged_store();

        let (repeating, outbound) = handled(
            run_cast(
                &store,
                repeating(),
                ClientOpcodeMessage::CMSG_CANCEL_AUTO_REPEAT_SPELL,
            )
            .unwrap(),
        );

        assert!(!repeating);
        assert_eq!(store.stop_attacks.lock().unwrap().as_slice(), &[CASTER]);
        assert!(
            outbound.is_empty(),
            "the engagement's on_delete relay is the one sender of the cancel signal"
        );
    }

    #[test]
    fn cancelling_with_no_active_loop_clears_the_state_without_a_durable_stop() {
        let store = ranged_store();

        let (repeating, outbound) = handled(
            run_cast(
                &store,
                session(),
                ClientOpcodeMessage::CMSG_CANCEL_AUTO_REPEAT_SPELL,
            )
            .unwrap(),
        );

        assert!(!repeating);
        assert!(
            store.stop_attacks.lock().unwrap().is_empty(),
            "a late cancel must not delete a just-armed melee engagement"
        );
        assert!(outbound.is_empty());
    }

    fn armed_with(launcher: ItemTemplateView, ammo: Vec<(u8, u32, u32)>) -> InMemoryCasts {
        let mut items = vec![owned(17, launcher.entry, 1)];
        let mut templates = vec![launcher];
        for (slot, entry, stack_count) in ammo {
            items.push(owned(slot, entry, stack_count));
            templates.push(projectile(entry, entry * 10));
        }
        InMemoryCasts {
            items,
            templates,
            ..ranged_store()
        }
    }

    fn activate_ammo(store: &InMemoryCasts) -> Option<(u32, u32)> {
        let (_, outbound) =
            handled(run_cast(store, session(), cast(AUTO_SHOT, unit_targets(88))).unwrap());
        ammo_block(&outbound)
    }

    #[test]
    fn every_launcher_class_nocks_its_projectile() {
        for subclass in [2u8, 3, 18] {
            let store = armed_with(launcher(100, subclass), vec![(5, 200, 40)]);

            assert_eq!(
                activate_ammo(&store),
                Some((2000, 24)),
                "weapon subclass {subclass} is a bow, gun or crossbow"
            );
        }
    }

    #[test]
    fn a_wand_omits_the_ammo_block() {
        let store = armed_with(launcher(100, 19), vec![(5, 200, 40)]);

        assert_eq!(
            activate_ammo(&store),
            None,
            "a wand fires its own bolt and renders it itself"
        );
    }

    #[test]
    fn an_empty_ranged_slot_omits_the_ammo_block() {
        let store = InMemoryCasts {
            items: vec![owned(5, 200, 40)],
            templates: vec![projectile(200, 2000)],
            ..ranged_store()
        };

        assert_eq!(activate_ammo(&store), None);
    }

    #[test]
    fn a_launcher_with_no_projectile_omits_the_ammo_block() {
        let store = armed_with(launcher(100, 2), Vec::new());

        assert_eq!(
            activate_ammo(&store),
            None,
            "the durable loop decides whether a shot can occur"
        );
    }

    #[test]
    fn a_spent_projectile_stack_is_never_nocked() {
        let store = armed_with(launcher(100, 2), vec![(5, 200, 0)]);

        assert_eq!(activate_ammo(&store), None);
    }

    #[test]
    fn the_lowest_slot_live_stack_wins() {
        let store = armed_with(
            launcher(100, 2),
            vec![(9, 300, 20), (4, 201, 60), (2, 200, 0)],
        );

        assert_eq!(
            activate_ammo(&store),
            Some((2010, 24)),
            "slot 2 is spent, so slot 4 is the deterministic pick"
        );
    }
    #[test]
    fn banked_projectiles_never_supply_the_activation() {
        let store = armed_with(launcher(100, 2), vec![(39, 200, 40), (120, 300, 20)]);
        assert_eq!(activate_ammo(&store), Some((3000, 24)));
        let bank_only = armed_with(launcher(100, 2), vec![(39, 200, 40)]);
        assert_eq!(activate_ammo(&bank_only), None);
    }
}
