use anyhow::Result;
use spacetimedb_sdk::Table;

use crate::stdb::bindings::*;
use crate::stdb::connection::call_reducer;
use crate::stdb::Coordinator;
use crate::world::{Actor, MailStore};

impl MailStore for Coordinator {
    fn mail_list(&self, recipient_guid: u64) -> Result<Vec<crate::codec::MailView>> {
        let guard = self.0.coord();
        let db = &guard.conn.db;
        Ok(db
            .game_mail()
            .iter()
            .filter(|m| m.recipient_guid == recipient_guid)
            .map(|m| mail_view(db, m))
            .collect())
    }

    fn mail_by_id(&self, mail_id: u64) -> Result<Option<crate::codec::MailView>> {
        let guard = self.0.coord();
        let db = &guard.conn.db;
        Ok(db.game_mail().id().find(&mail_id).map(|m| mail_view(db, m)))
    }

    fn realm_account_name(&self, character_guid: u64) -> Result<Option<String>> {
        Ok(self.realm_account_name(character_guid))
    }

    fn mailbox_in_range(&self, mailbox_guid: u64, player_guid: u64) -> Result<bool> {
        let guard = self.0.coord();
        let db = &guard.conn.db;
        let (Some(go), Some(player)) = (
            db.game_gameobject().guid().find(&mailbox_guid),
            db.game_world_entity().guid().find(&player_guid),
        ) else {
            return Ok(false);
        };
        let is_mailbox = db
            .game_gameobject_template()
            .entry()
            .find(&go.template_entry)
            .is_some_and(|t| t.type_id == lyracore_shared::mail::MAILBOX_GO_TYPE);
        if !is_mailbox || go.map_id != player.map_id || go.instance_id != player.instance_id {
            return Ok(false);
        }
        let (dx, dy, dz) = (go.x - player.x, go.y - player.y, go.z - player.z);
        Ok(dx * dx + dy * dy + dz * dz <= lyracore_shared::mail::MAILBOX_RANGE_SQ)
    }

    fn mail_mark_read(&self, recipient: Actor, mail_id: u64) -> Result<()> {
        call_reducer!(
            self.0.call_pipe().conn.reducers,
            "realm_mail_mark_read",
            realm_mail_mark_read_then(self.session_actor(recipient), mail_id)
        )
    }

    fn mail_delete(&self, recipient: Actor, mail_id: u64) -> Result<()> {
        call_reducer!(
            self.0.call_pipe().conn.reducers,
            "realm_mail_delete",
            realm_mail_delete_then(self.session_actor(recipient), mail_id)
        )
    }

    fn mail_return(&self, recipient: Actor, mail_id: u64, same_account: bool) -> Result<()> {
        call_reducer!(
            self.0.call_pipe().conn.reducers,
            "realm_mail_return",
            realm_mail_return_then(self.session_actor(recipient), mail_id, same_account)
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn mail_send(
        &self,
        sender: Actor,
        recipient_guid: u64,
        subject: String,
        body: String,
        money: u32,
        cod: u32,
        item_guid: u64,
        same_account: bool,
    ) -> Result<()> {
        call_reducer!(
            self.0.call_pipe().conn.reducers,
            "realm_mail_send",
            realm_mail_send_then(
                self.session_actor(sender),
                recipient_guid,
                subject,
                body,
                money,
                cod,
                item_guid,
                same_account
            )
        )
    }

    fn mail_take_money(&self, recipient: Actor, mail_id: u64) -> Result<()> {
        call_reducer!(
            self.0.call_pipe().conn.reducers,
            "realm_mail_take_money",
            realm_mail_take_money_then(self.session_actor(recipient), mail_id)
        )
    }

    fn mail_take_item(&self, recipient: Actor, mail_id: u64) -> Result<()> {
        call_reducer!(
            self.0.call_pipe().conn.reducers,
            "realm_mail_take_item",
            realm_mail_take_item_then(self.session_actor(recipient), mail_id)
        )
    }

    fn mail_item_room(&self, payee: Actor) -> Result<()> {
        call_reducer!(
            self.0.call_pipe().conn.reducers,
            "realm_mail_item_room",
            realm_mail_item_room_then(self.session_actor(payee))
        )
    }

    fn mail_copy_text(&self, recipient: Actor, mail_id: u64) -> Result<()> {
        call_reducer!(
            self.0.call_pipe().conn.reducers,
            "realm_mail_copy_text",
            realm_mail_copy_text_then(self.session_actor(recipient), mail_id)
        )
    }

    fn mail_grant_letter(&self, payee: Actor, item_text_id: u32) -> Result<()> {
        call_reducer!(
            self.0.call_pipe().conn.reducers,
            "gw_mail_grant_letter",
            gw_mail_grant_letter_then(self.session_actor(payee), item_text_id)
        )
    }

    fn mail_mark_letter_granted(&self, recipient: Actor, mail_id: u64) -> Result<()> {
        call_reducer!(
            self.0.call_pipe().conn.reducers,
            "realm_mail_mark_letter_granted",
            realm_mail_mark_letter_granted_then(self.session_actor(recipient), mail_id)
        )
    }

    fn item_text(&self, item_text_id: u32) -> Result<Option<String>> {
        let guard = self.0.coord();
        Ok(guard
            .conn
            .db
            .game_item_text()
            .id()
            .find(&item_text_id)
            .map(|t| t.text))
    }

    fn owns_item_with_text(
        &self,
        owner_guid: u64,
        item_text_id: u32,
        hint_item_guid: u64,
    ) -> Result<bool> {
        if item_text_id == 0 || hint_item_guid == 0 {
            return Ok(false);
        }
        let guard = self.0.coord();
        let db = &guard.conn.db;
        let owns = db
            .game_item_instance()
            .guid()
            .find(&hint_item_guid)
            .is_some_and(|item| item.owner_guid == owner_guid && item.item_text_id == item_text_id);
        Ok(owns)
    }

    #[allow(clippy::too_many_arguments)]
    fn mail_fence(
        &self,
        escrow_id: u64,
        sender: Actor,
        recipient_guid: u64,
        subject: String,
        body: String,
        money: u32,
        postage: u32,
        item_guid: u64,
        cod: u32,
        cod_source_mail_id: u64,
        same_account: bool,
    ) -> Result<()> {
        call_reducer!(
            self.0.call_pipe().conn.reducers,
            "realm_mail_fence",
            realm_mail_fence_then(
                escrow_id,
                self.session_actor(sender),
                recipient_guid,
                subject,
                body,
                money,
                postage,
                item_guid,
                cod,
                cod_source_mail_id,
                same_account
            )
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn mail_commit(
        &self,
        escrow_id: u64,
        sender: Actor,
        recipient_guid: u64,
        subject: String,
        body: String,
        money: u32,
        item: crate::world::mail::AttachedItem,
        cod: u32,
        cod_source_mail_id: u64,
        delivery_delay_secs: u32,
        reward: Option<lyracore_shared::mail::RewardHeader>,
    ) -> Result<()> {
        let (sender_kind, sender_entry, mail_template_id) =
            lyracore_shared::mail::RewardHeader::columns(reward);
        call_reducer!(
            self.0.call_pipe().conn.reducers,
            "realm_mail_commit",
            realm_mail_commit_then(
                escrow_id,
                self.session_actor(sender),
                recipient_guid,
                subject,
                body,
                money,
                item.entry,
                item.stack_count,
                item.durability,
                item.enchant_id,
                item.soulbound,
                item.random_property_id,
                cod,
                cod_source_mail_id,
                delivery_delay_secs,
                sender_kind,
                sender_entry,
                mail_template_id,
                item.item_text_id
            )
        )
    }

    fn mail_take_money_fence(
        &self,
        escrow_id: u64,
        payee: Actor,
        mail_id: u64,
        expect_money: u32,
    ) -> Result<()> {
        call_reducer!(
            self.0.call_pipe().conn.reducers,
            "realm_mail_take_money_fence",
            realm_mail_take_money_fence_then(
                escrow_id,
                self.session_actor(payee),
                mail_id,
                expect_money
            )
        )
    }

    fn mail_payout(&self, escrow_id: u64, payee: Actor, mail_id: u64, amount: u32) -> Result<()> {
        call_reducer!(
            self.0.call_pipe().conn.reducers,
            "realm_mail_payout",
            realm_mail_payout_then(escrow_id, self.session_actor(payee), mail_id, amount)
        )
    }

    fn mail_take_item_fence(
        &self,
        escrow_id: u64,
        payee: Actor,
        mail_id: u64,
        expect_entry: u32,
    ) -> Result<()> {
        call_reducer!(
            self.0.call_pipe().conn.reducers,
            "realm_mail_take_item_fence",
            realm_mail_take_item_fence_then(
                escrow_id,
                self.session_actor(payee),
                mail_id,
                expect_entry
            )
        )
    }

    fn mail_item_payout(
        &self,
        escrow_id: u64,
        payee: Actor,
        mail_id: u64,
        item: crate::world::mail::AttachedItem,
    ) -> Result<()> {
        call_reducer!(
            self.0.call_pipe().conn.reducers,
            "realm_mail_item_payout",
            realm_mail_item_payout_then(
                escrow_id,
                self.session_actor(payee),
                mail_id,
                item.entry,
                item.stack_count,
                item.durability,
                item.enchant_id,
                item.soulbound,
                item.random_property_id,
                item.item_text_id
            )
        )
    }

    fn mail_confirm_delivery(&self, escrow_id: u64) -> Result<()> {
        call_reducer!(
            self.0.call_pipe().conn.reducers,
            "realm_mail_confirm_delivery",
            realm_mail_confirm_delivery_then(escrow_id, self.owner_actor())
        )
    }

    fn mail_settle(&self, escrow_id: u64) -> Result<()> {
        call_reducer!(
            self.0.call_pipe().conn.reducers,
            "realm_mail_settle",
            realm_mail_settle_then(escrow_id, self.owner_actor())
        )
    }

    fn mail_escrows_of(&self, sender_guid: u64) -> Result<Vec<crate::world::mail::HeldEscrow>> {
        let guard = self.0.coord();
        let ids = guard.mail_escrows.read().unwrap().of(sender_guid);
        let escrows = guard.conn.db.game_mail_escrow();
        Ok(ids
            .into_iter()
            .filter_map(|id| escrows.escrow_id().find(&id))
            .filter_map(|e| {
                let reward = lyracore_shared::mail::RewardHeader::from_columns(
                    e.sender_kind,
                    e.sender_entry,
                    e.mail_template_id,
                )
                .map_err(|refusal| {
                    log::error!("mail escrow {}: not driven: {refusal}", e.escrow_id)
                })
                .ok()?;
                Some((e, reward))
            })
            .map(|(e, reward)| crate::world::mail::HeldEscrow {
                escrow_id: e.escrow_id,
                recipient_guid: e.recipient_guid,
                subject: e.subject,
                body: e.body,
                money: e.money,
                postage: e.postage,
                payout: e.payout,
                mail_id: e.mail_id,
                item: crate::world::mail::AttachedItem {
                    entry: e.item_entry,
                    stack_count: e.item_stack_count,
                    durability: e.item_durability,
                    enchant_id: e.item_enchant_id,
                    soulbound: e.item_soulbound,
                    random_property_id: e.random_property_id,
                    item_text_id: e.item_text_id,
                },
                cod: e.cod,
                delivery_delay_secs: e.delivery_delay_secs,
                reward,
            })
            .collect())
    }
}

fn mail_view(db: &RemoteTables, m: Mail) -> crate::codec::MailView {
    crate::codec::MailView {
        id: m.id,
        sender_guid: m.sender_guid,
        subject: m.subject,
        body: m.body,
        item_entry: m.item_entry,
        item_stack_count: m.item_stack_count,
        item_durability: m.item_durability,
        max_durability: db
            .game_item_template()
            .entry()
            .find(&m.item_entry)
            .map(|t| t.max_durability)
            .unwrap_or(0),
        item_enchant_id: m.item_enchant_id,
        item_soulbound: m.item_soulbound,
        random_property_id: m.random_property_id,
        money: m.money,
        cod: m.cod,
        was_read: m.was_read,
        created_at_secs: m.created_at.to_micros_since_unix_epoch() / 1_000_000,
        sender_kind: m.sender_kind,
        sender_entry: m.sender_entry,
        check_flags: m.check_flags,
        mail_template_id: m.mail_template_id,
        deliver_secs: m
            .deliver_micros
            .saturating_add(999_999)
            .div_euclid(1_000_000),
    }
}
