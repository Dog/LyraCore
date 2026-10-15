use super::super::*;

fn refused(reason: impl std::fmt::Display) -> anyhow::Error {
    crate::stdb::ReducerCallError::refused("mail", &reason.to_string()).into()
}

pub(crate) fn delivery_delay_secs(has_item: bool, same_account: bool) -> u32 {
    if has_item && !same_account {
        3_600
    } else {
        0
    }
}

pub(crate) fn delivered_after(delay_secs: u32) -> i64 {
    if delay_secs == 0 {
        0
    } else {
        crate::world::mail::now_secs() + i64::from(delay_secs)
    }
}

#[derive(Default)]
pub(crate) struct MailState {
    pub(crate) mails: std::sync::Mutex<Vec<(u64, codec::MailView)>>,
    pub(crate) mailboxes: Vec<u64>,
    pub(crate) purses: std::sync::Mutex<Vec<(u64, u32)>>,
    #[allow(clippy::type_complexity)]
    pub(crate) sent_mail: std::sync::Mutex<Vec<(u64, u64, String, String, u32)>>,
    #[allow(clippy::type_complexity)]
    pub(crate) mail_escrows: std::sync::Mutex<Vec<(u64, crate::world::mail::HeldEscrow)>>,
    pub(crate) mail_escrow_reads_before_visible: std::sync::atomic::AtomicUsize,
    pub(crate) realm_accounts: std::sync::Mutex<Vec<(u64, String)>>,
    pub(crate) same_account_seen: std::sync::Mutex<Vec<(&'static str, bool)>>,
    pub(crate) attested: std::sync::Mutex<Vec<(u64, bool)>>,
    pub(crate) mail_receipts: std::sync::Mutex<Vec<(u64, u64)>>,
    pub(crate) item_texts: std::sync::Mutex<Vec<(u32, String)>>,
    pub(crate) granted_letters: std::sync::Mutex<Vec<(u64, u32)>>,
    pub(crate) mail_item_text_ids: std::sync::Mutex<Vec<(u64, u32)>>,
    pub(crate) mail_kill_at: std::sync::Mutex<Option<String>>,
    pub(crate) mail_items: std::sync::Mutex<Vec<(u64, u64, crate::world::mail::AttachedItem)>>,
    pub(crate) bags_full: std::sync::atomic::AtomicBool,
}

impl MailStore for WorldFake {
    fn mail_list(&self, recipient_guid: u64) -> Result<Vec<codec::MailView>> {
        self.rec("mail_list");
        Ok(self
            .mail
            .mails
            .lock()
            .unwrap()
            .iter()
            .filter(|(to, _)| *to == recipient_guid)
            .map(|(_, m)| m.clone())
            .collect())
    }

    fn mail_by_id(&self, mail_id: u64) -> Result<Option<codec::MailView>> {
        Ok(self
            .mail
            .mails
            .lock()
            .unwrap()
            .iter()
            .find(|(_, m)| m.id == mail_id)
            .map(|(_, m)| m.clone()))
    }

    fn realm_account_name(&self, character_guid: u64) -> Result<Option<String>> {
        Ok(self
            .mail
            .realm_accounts
            .lock()
            .unwrap()
            .iter()
            .find(|(guid, _)| *guid == character_guid)
            .map(|(_, name)| name.clone()))
    }

    fn mailbox_in_range(&self, mailbox_guid: u64, _player_guid: u64) -> Result<bool> {
        self.rec("mailbox_in_range");
        Ok(self.mail.mailboxes.contains(&mailbox_guid))
    }

    fn mail_mark_read(&self, recipient: Actor, mail_id: u64) -> Result<()> {
        let recipient_guid = recipient.guid();
        self.rec("mail_mark_read");
        let mut mails = self.mail.mails.lock().unwrap();
        let now = crate::world::mail::now_secs();
        match mails
            .iter_mut()
            .find(|(to, m)| *to == recipient_guid && m.id == mail_id && m.is_delivered(now))
        {
            Some((_, m)) => {
                m.was_read = true;
                Ok(())
            }
            None => Err(refused(lyracore_shared::mail::NOT_YOUR_MAIL)),
        }
    }

    fn mail_delete(&self, recipient: Actor, mail_id: u64) -> Result<()> {
        let recipient_guid = recipient.guid();
        self.rec("mail_delete");
        let mut mails = self.mail.mails.lock().unwrap();
        let now = crate::world::mail::now_secs();
        let Some(at) = mails
            .iter()
            .position(|(to, m)| *to == recipient_guid && m.id == mail_id && m.is_delivered(now))
        else {
            return Err(refused(lyracore_shared::mail::NOT_YOUR_MAIL));
        };
        if mails[at].1.cod > 0 {
            return Err(refused(lyracore_shared::mail::COD_MAIL_UNDELETABLE));
        }
        mails.remove(at);
        Ok(())
    }

    fn mail_return(&self, recipient: Actor, mail_id: u64, same_account: bool) -> Result<()> {
        let recipient_guid = recipient.guid();
        self.rec("mail_return");
        self.saw_same_account("mail_return", same_account);
        let mut mails = self.mail.mails.lock().unwrap();
        let now = crate::world::mail::now_secs();
        let Some((to, m)) = mails
            .iter_mut()
            .find(|(to, m)| *to == recipient_guid && m.id == mail_id && m.is_delivered(now))
        else {
            return Err(refused(lyracore_shared::mail::NOT_YOUR_MAIL));
        };
        let lyracore_shared::mail::MailSender::Character(sender @ 1..) = m.sender() else {
            return Err(refused(lyracore_shared::mail::NO_SENDER_TO_RETURN_TO));
        };
        if m.check_flags & lyracore_shared::mail::CHECK_MASK_RETURNED != 0 {
            return Err(refused(lyracore_shared::mail::ALREADY_RETURNED));
        }
        m.sender_guid = recipient_guid;
        m.was_read = false;
        m.cod = 0;
        m.check_flags = lyracore_shared::mail::CHECK_MASK_RETURNED;
        m.deliver_secs = now + i64::from(delivery_delay_secs(m.item_entry != 0, same_account));
        *to = sender;
        Ok(())
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
        let sender_guid = sender.guid();
        self.rec("mail_send");
        self.saw_same_account("mail_send", same_account);
        let item = self.detach(sender_guid, item_guid)?;
        self.debit(sender_guid, lyracore_shared::mail::total_cost(money))?;
        self.mail.sent_mail.lock().unwrap().push((
            sender_guid,
            recipient_guid,
            subject.clone(),
            body.clone(),
            money,
        ));
        self.write_mail(
            sender_guid,
            recipient_guid,
            subject,
            body,
            money,
            cod,
            &item,
            false,
            delivery_delay_secs(!item.is_empty(), same_account),
        );
        Ok(())
    }

    fn mail_take_item(&self, recipient: Actor, mail_id: u64) -> Result<()> {
        let recipient_guid = recipient.guid();
        let (item, settlement) = {
            let mails = self.mail.mails.lock().unwrap();
            let now = crate::world::mail::now_secs();
            let Some((_, m)) = mails
                .iter()
                .find(|(to, m)| *to == recipient_guid && m.id == mail_id && m.is_delivered(now))
            else {
                return Err(refused(lyracore_shared::mail::NOT_YOUR_MAIL));
            };
            if m.item_entry == 0 {
                return Err(refused(lyracore_shared::mail::NOTHING_TO_TAKE));
            }
            (
                crate::world::mail::AttachedItem {
                    entry: m.item_entry,
                    stack_count: m.item_stack_count,
                    durability: m.item_durability,
                    enchant_id: m.item_enchant_id,
                    soulbound: m.item_soulbound,
                    random_property_id: m.random_property_id,
                    item_text_id: self.attached_text_id(mail_id),
                },
                lyracore_shared::mail::cod_settlement(
                    m.cod,
                    m.sender_guid,
                    &m.subject,
                    recipient_guid,
                ),
            )
        };
        self.rec("mail_take_item");
        if let Some(s) = &settlement {
            self.debit(s.payer_guid, s.copper)
                .map_err(|_| refused(lyracore_shared::mail::COD_NOT_AFFORDABLE))?;
        }
        if let Err(e) = self.store_snapshot(recipient_guid, &item) {
            // The Fake lacks rollback, so undo this write to match the Module's transaction.
            if let Some(s) = &settlement {
                self.credit(s.payer_guid, s.copper);
            }
            return Err(e);
        }
        let mut mails = self.mail.mails.lock().unwrap();
        if let Some((_, m)) = mails
            .iter_mut()
            .find(|(to, m)| *to == recipient_guid && m.id == mail_id)
        {
            m.item_entry = 0;
            m.item_stack_count = 0;
            m.item_durability = 0;
            m.item_enchant_id = 0;
            m.item_soulbound = false;
            m.cod = 0;
        }
        drop(mails);
        self.take_attached_text_id(mail_id);
        if let Some(s) = settlement {
            self.write_mail(
                s.payer_guid,
                s.payee_guid,
                s.subject,
                String::new(),
                s.copper,
                0,
                &crate::world::mail::AttachedItem::default(),
                true,
                0,
            );
        }
        Ok(())
    }

    fn mail_item_room(&self, _payee: Actor) -> Result<()> {
        self.rec("mail_item_room");
        if self
            .mail
            .bags_full
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(refused(lyracore_shared::mail::INVENTORY_FULL));
        }
        Ok(())
    }

    fn mail_copy_text(&self, recipient: Actor, mail_id: u64) -> Result<()> {
        let recipient_guid = recipient.guid();
        self.rec("mail_copy_text");
        let mut mails = self.mail.mails.lock().unwrap();
        let now = crate::world::mail::now_secs();
        let Some((_, m)) = mails
            .iter_mut()
            .find(|(to, m)| *to == recipient_guid && m.id == mail_id && m.is_delivered(now))
        else {
            return Err(refused(lyracore_shared::mail::NOT_YOUR_MAIL));
        };
        if m.body.is_empty() {
            return Err(refused("mail: this mail has no text to copy"));
        }
        if m.check_flags & lyracore_shared::mail::CHECK_FLAG_LETTER_GRANTED != 0 {
            return Err(refused("mail: this letter was already made permanent"));
        }
        m.check_flags |= lyracore_shared::mail::CHECK_MASK_COPIED;
        let text_id = lyracore_shared::mail::item_text_id_for(m.id, &m.body);
        let text = m.body.clone();
        drop(mails);
        let mut texts = self.mail.item_texts.lock().unwrap();
        if !texts.iter().any(|(id, _)| *id == text_id) {
            texts.push((text_id, text));
        }
        Ok(())
    }

    fn mail_grant_letter(&self, payee: Actor, item_text_id: u32) -> Result<()> {
        let payee_guid = payee.guid();
        self.rec("mail_grant_letter");
        if self
            .mail
            .granted_letters
            .lock()
            .unwrap()
            .iter()
            .any(|&(guid, id)| guid == payee_guid && id == item_text_id)
        {
            return Ok(());
        }
        if self
            .mail
            .bags_full
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(refused(lyracore_shared::mail::INVENTORY_FULL));
        }
        self.mail
            .granted_letters
            .lock()
            .unwrap()
            .push((payee_guid, item_text_id));
        Ok(())
    }

    fn mail_mark_letter_granted(&self, recipient: Actor, mail_id: u64) -> Result<()> {
        let recipient_guid = recipient.guid();
        self.rec("mail_mark_letter_granted");
        let mut mails = self.mail.mails.lock().unwrap();
        let Some((_, m)) = mails
            .iter_mut()
            .find(|(to, m)| *to == recipient_guid && m.id == mail_id)
        else {
            return Err(refused(lyracore_shared::mail::NOT_YOUR_MAIL));
        };
        m.check_flags |= lyracore_shared::mail::CHECK_FLAG_LETTER_GRANTED;
        Ok(())
    }

    fn item_text(&self, item_text_id: u32) -> Result<Option<String>> {
        self.rec("item_text");
        Ok(self
            .mail
            .item_texts
            .lock()
            .unwrap()
            .iter()
            .find(|(id, _)| *id == item_text_id)
            .map(|(_, text)| text.clone()))
    }

    fn owns_item_with_text(
        &self,
        owner_guid: u64,
        item_text_id: u32,
        _hint_item_guid: u64,
    ) -> Result<bool> {
        self.rec("owns_item_with_text");
        Ok(item_text_id != 0
            && self
                .mail
                .granted_letters
                .lock()
                .unwrap()
                .iter()
                .any(|&(guid, id)| guid == owner_guid && id == item_text_id))
    }

    fn mail_take_money(&self, recipient: Actor, mail_id: u64) -> Result<()> {
        let recipient_guid = recipient.guid();
        self.rec("mail_take_money");
        let money = {
            let mut mails = self.mail.mails.lock().unwrap();
            let now = crate::world::mail::now_secs();
            let Some((_, m)) = mails
                .iter_mut()
                .find(|(to, m)| *to == recipient_guid && m.id == mail_id && m.is_delivered(now))
            else {
                return Err(refused(lyracore_shared::mail::NOT_YOUR_MAIL));
            };
            if m.money == 0 {
                return Err(refused(lyracore_shared::mail::NOTHING_TO_TAKE));
            }
            std::mem::take(&mut m.money)
        };
        self.credit(recipient_guid, money);
        Ok(())
    }

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
        let sender_guid = sender.guid();
        self.rec("mail_fence");
        self.saw_same_account("mail_fence", same_account);
        self.mail_kill("mail_fence")?;
        let escrows = self.mail.mail_escrows.lock().unwrap();
        if escrows.iter().any(|(_, e)| e.escrow_id == escrow_id) {
            return Ok(());
        }
        drop(escrows);
        let item = self.detach(sender_guid, item_guid)?;
        self.debit(sender_guid, money.saturating_add(postage))?;
        let delivery_delay_secs = delivery_delay_secs(!item.is_empty(), same_account);
        self.mail.mail_escrows.lock().unwrap().push((
            sender_guid,
            crate::world::mail::HeldEscrow {
                escrow_id,
                recipient_guid,
                subject,
                body,
                money,
                postage,
                payout: false,
                mail_id: cod_source_mail_id,
                item,
                cod,
                delivery_delay_secs,
                reward: None,
            },
        ));
        self.mail.attested.lock().unwrap().push((escrow_id, false));
        Ok(())
    }

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
        let sender_guid = sender.guid();
        self.rec("mail_commit");
        self.mail_kill("mail_commit")?;
        let mut receipts = self.mail.mail_receipts.lock().unwrap();
        if receipts.iter().any(|(id, _)| *id == escrow_id) {
            return Ok(());
        }
        if cod_source_mail_id != 0 {
            let now = crate::world::mail::now_secs();
            let owed = self.mail.mails.lock().unwrap().iter().any(|(to, m)| {
                m.id == cod_source_mail_id && *to == sender_guid && m.cod > 0 && m.is_delivered(now)
            });
            if !owed {
                return Err(refused(format!(
                    "mail {cod_source_mail_id} owes {sender_guid} no delivered price"
                )));
            }
        }
        receipts.push((escrow_id, recipient_guid));
        drop(receipts);
        self.mail.sent_mail.lock().unwrap().push((
            sender_guid,
            recipient_guid,
            subject.clone(),
            body.clone(),
            money,
        ));
        self.write_mail(
            sender_guid,
            recipient_guid,
            subject,
            body,
            money,
            cod,
            &item,
            cod_source_mail_id != 0,
            if cod_source_mail_id != 0 {
                0
            } else {
                delivery_delay_secs
            },
        );
        if let Some(header) = reward {
            let (sender_kind, sender_guid, sender_entry) = header.giver.sender().columns();
            let mut mails = self.mail.mails.lock().unwrap();
            if let Some((_, m)) = mails.iter_mut().max_by_key(|(_, m)| m.id) {
                m.sender_guid = sender_guid;
                m.sender_kind = sender_kind;
                m.sender_entry = sender_entry;
                m.mail_template_id = header.mail_template_id;
                m.check_flags = lyracore_shared::mail::CHECK_MASK_HAS_BODY;
            }
        }
        if cod_source_mail_id != 0 {
            if let Some((_, m)) = self
                .mail
                .mails
                .lock()
                .unwrap()
                .iter_mut()
                .find(|(_, m)| m.id == cod_source_mail_id)
            {
                m.cod = 0;
            }
        }
        Ok(())
    }

    fn mail_take_money_fence(
        &self,
        escrow_id: u64,
        payee: Actor,
        mail_id: u64,
        expect_money: u32,
    ) -> Result<()> {
        let payee_guid = payee.guid();
        self.rec("mail_take_money_fence");
        self.mail_kill("mail_take_money_fence")?;
        if self
            .mail
            .mail_escrows
            .lock()
            .unwrap()
            .iter()
            .any(|(_, e)| e.escrow_id == escrow_id)
        {
            return Ok(());
        }
        let money = {
            let mut mails = self.mail.mails.lock().unwrap();
            let now = crate::world::mail::now_secs();
            let Some((_, m)) = mails
                .iter_mut()
                .find(|(to, m)| *to == payee_guid && m.id == mail_id && m.is_delivered(now))
            else {
                return Err(refused(lyracore_shared::mail::NOT_YOUR_MAIL));
            };
            if m.money == 0 {
                return Err(refused(lyracore_shared::mail::NOTHING_TO_TAKE));
            }
            if m.money != expect_money {
                return Err(refused(
                    "refusing to fence an amount the payout would not match",
                ));
            }
            std::mem::take(&mut m.money)
        };
        self.mail.mail_escrows.lock().unwrap().push((
            payee_guid,
            crate::world::mail::HeldEscrow {
                escrow_id,
                recipient_guid: payee_guid,
                subject: String::new(),
                body: String::new(),
                money,
                postage: 0,
                payout: true,
                mail_id,
                item: crate::world::mail::AttachedItem::default(),
                cod: 0,
                delivery_delay_secs: 0,
                reward: None,
            },
        ));
        self.mail.attested.lock().unwrap().push((escrow_id, false));
        Ok(())
    }

    fn mail_take_item_fence(
        &self,
        escrow_id: u64,
        payee: Actor,
        mail_id: u64,
        expect_entry: u32,
    ) -> Result<()> {
        let payee_guid = payee.guid();
        self.rec("mail_take_item_fence");
        self.mail_kill("mail_take_item_fence")?;
        if self
            .mail
            .mail_escrows
            .lock()
            .unwrap()
            .iter()
            .any(|(_, e)| e.escrow_id == escrow_id)
        {
            return Ok(());
        }
        let item = {
            let mut mails = self.mail.mails.lock().unwrap();
            let now = crate::world::mail::now_secs();
            let Some((_, m)) = mails
                .iter_mut()
                .find(|(to, m)| *to == payee_guid && m.id == mail_id && m.is_delivered(now))
            else {
                return Err(refused(lyracore_shared::mail::NOT_YOUR_MAIL));
            };
            if m.item_entry == 0 {
                return Err(refused(lyracore_shared::mail::NOTHING_TO_TAKE));
            }
            if m.item_entry != expect_entry {
                return Err(refused(
                    "refusing to fence an item the grant would not match",
                ));
            }
            let item = crate::world::mail::AttachedItem {
                entry: m.item_entry,
                stack_count: m.item_stack_count,
                durability: m.item_durability,
                enchant_id: m.item_enchant_id,
                soulbound: m.item_soulbound,
                random_property_id: m.random_property_id,
                item_text_id: self.take_attached_text_id(mail_id),
            };
            m.item_entry = 0;
            m.item_stack_count = 0;
            m.item_durability = 0;
            m.item_enchant_id = 0;
            m.item_soulbound = false;
            item
        };
        self.mail.mail_escrows.lock().unwrap().push((
            payee_guid,
            crate::world::mail::HeldEscrow {
                escrow_id,
                recipient_guid: payee_guid,
                subject: String::new(),
                body: String::new(),
                money: 0,
                postage: 0,
                payout: true,
                mail_id,
                item,
                cod: 0,
                delivery_delay_secs: 0,
                reward: None,
            },
        ));
        self.mail.attested.lock().unwrap().push((escrow_id, false));
        Ok(())
    }

    fn mail_item_payout(
        &self,
        escrow_id: u64,
        payee: Actor,
        _mail_id: u64,
        item: crate::world::mail::AttachedItem,
    ) -> Result<()> {
        let payee_guid = payee.guid();
        self.rec("mail_item_payout");
        self.mail_kill("mail_item_payout")?;
        let receipts = self.mail.mail_receipts.lock().unwrap();
        if receipts.iter().any(|(id, _)| *id == escrow_id) {
            return Ok(());
        }
        drop(receipts);
        self.store_snapshot(payee_guid, &item)?;
        self.mail
            .mail_receipts
            .lock()
            .unwrap()
            .push((escrow_id, payee_guid));
        Ok(())
    }

    fn mail_payout(&self, escrow_id: u64, payee: Actor, _mail_id: u64, amount: u32) -> Result<()> {
        let payee_guid = payee.guid();
        self.rec("mail_payout");
        self.mail_kill("mail_payout")?;
        let mut receipts = self.mail.mail_receipts.lock().unwrap();
        if receipts.iter().any(|(id, _)| *id == escrow_id) {
            return Ok(());
        }
        if !self
            .mail
            .purses
            .lock()
            .unwrap()
            .iter()
            .any(|(g, _)| *g == payee_guid)
        {
            return Err(refused(lyracore_shared::mail::NOT_IN_WORLD));
        }
        receipts.push((escrow_id, payee_guid));
        drop(receipts);
        self.credit(payee_guid, amount);
        Ok(())
    }

    fn mail_confirm_delivery(&self, escrow_id: u64) -> Result<()> {
        self.rec("mail_confirm_delivery");
        self.mail_kill("mail_confirm_delivery")?;
        let mut attested = self.mail.attested.lock().unwrap();
        match attested.iter_mut().find(|(id, _)| *id == escrow_id) {
            Some((_, done)) => {
                *done = true;
                Ok(())
            }
            None => Err(refused(format!(
                "mail escrow {escrow_id}: nothing fenced here"
            ))),
        }
    }

    fn mail_settle(&self, escrow_id: u64) -> Result<()> {
        self.rec("mail_settle");
        self.mail_kill("mail_settle")?;
        let attested = self
            .mail
            .attested
            .lock()
            .unwrap()
            .iter()
            .find(|(id, _)| *id == escrow_id)
            .map(|(_, done)| *done);
        match attested {
            None => Ok(()),
            Some(false) => Err(refused(format!(
                "mail escrow {escrow_id}: delivery not attested — refusing to destroy the fence"
            ))),
            Some(true) => {
                self.mail
                    .mail_escrows
                    .lock()
                    .unwrap()
                    .retain(|(_, e)| e.escrow_id != escrow_id);
                self.mail
                    .attested
                    .lock()
                    .unwrap()
                    .retain(|(id, _)| *id != escrow_id);
                Ok(())
            }
        }
    }

    fn mail_escrows_of(&self, sender_guid: u64) -> Result<Vec<crate::world::mail::HeldEscrow>> {
        self.rec("mail_escrows_of");
        if self
            .mail
            .mail_escrow_reads_before_visible
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |left| left.checked_sub(1),
            )
            .is_ok()
        {
            return Ok(Vec::new());
        }
        Ok(self
            .mail
            .mail_escrows
            .lock()
            .unwrap()
            .iter()
            .filter(|(owner, _)| *owner == sender_guid)
            .map(|(_, e)| e.clone())
            .collect())
    }
}

impl WorldFake {
    pub(crate) fn mail_kill(&self, step: &str) -> Result<()> {
        if self.mail.mail_kill_at.lock().unwrap().as_deref() == Some(step) {
            log::info!(
                "injected: the gateway died before {step} on {}",
                self.topology.shard
            );
            return Err(crate::stdb::ReducerCallError::transport_lost(step).into());
        }
        Ok(())
    }

    pub(crate) fn debit(&self, guid: u64, copper: u32) -> Result<()> {
        let mut purses = self.mail.purses.lock().unwrap();
        match purses.iter_mut().find(|(g, _)| *g == guid) {
            Some((_, money)) if *money >= copper => {
                *money -= copper;
                Ok(())
            }
            _ => Err(refused(lyracore_shared::mail::NOT_ENOUGH_MONEY)),
        }
    }

    pub(crate) fn credit(&self, guid: u64, copper: u32) {
        let mut purses = self.mail.purses.lock().unwrap();
        if let Some((_, money)) = purses.iter_mut().find(|(g, _)| *g == guid) {
            *money = money.saturating_add(copper);
        }
    }

    pub(crate) fn detach(
        &self,
        sender_guid: u64,
        item_guid: u64,
    ) -> Result<crate::world::mail::AttachedItem> {
        if item_guid == 0 {
            return Ok(crate::world::mail::AttachedItem::default());
        }
        let mut items = self.mail.mail_items.lock().unwrap();
        match items
            .iter()
            .position(|(g, owner, _)| *g == item_guid && *owner == sender_guid)
        {
            None => Err(refused(lyracore_shared::mail::NOT_YOUR_ITEM)),
            Some(i) if items[i].2.soulbound => {
                Err(refused(lyracore_shared::mail::ITEM_IS_SOULBOUND))
            }
            Some(i) => Ok(items.remove(i).2),
        }
    }

    pub(crate) fn store_snapshot(
        &self,
        owner_guid: u64,
        item: &crate::world::mail::AttachedItem,
    ) -> Result<()> {
        if self
            .mail
            .bags_full
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(refused(lyracore_shared::mail::INVENTORY_FULL));
        }
        let mut items = self.mail.mail_items.lock().unwrap();
        let guid = items.iter().map(|(g, _, _)| *g).max().unwrap_or(0) + 1;
        items.push((guid, owner_guid, item.clone()));
        Ok(())
    }

    pub(crate) fn bags_of(&self, owner: u64) -> Vec<crate::world::mail::AttachedItem> {
        self.mail
            .mail_items
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, o, _)| *o == owner)
            .map(|(_, _, i)| i.clone())
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn write_mail(
        &self,
        sender_guid: u64,
        recipient_guid: u64,
        subject: String,
        body: String,
        money: u32,
        cod: u32,
        item: &crate::world::mail::AttachedItem,
        cod_payment: bool,
        delay_secs: u32,
    ) {
        let check_flags = if cod_payment {
            lyracore_shared::mail::CHECK_MASK_COD_PAYMENT
        } else if body.is_empty() {
            lyracore_shared::mail::CHECK_MASK_COPIED
        } else {
            lyracore_shared::mail::CHECK_MASK_HAS_BODY
        };
        let mut mails = self.mail.mails.lock().unwrap();
        let id = mails.iter().map(|(_, m)| m.id).max().unwrap_or(0) + 1;
        mails.push((
            recipient_guid,
            codec::MailView {
                id,
                sender_guid,
                subject,
                body,
                money,
                cod,
                item_entry: item.entry,
                item_stack_count: item.stack_count,
                item_durability: item.durability,
                item_enchant_id: item.enchant_id,
                item_soulbound: item.soulbound,
                random_property_id: item.random_property_id,
                created_at_secs: 1_000,
                check_flags,
                deliver_secs: delivered_after(delay_secs),
                ..Default::default()
            },
        ));
        drop(mails);
        if item.item_text_id != 0 {
            self.mail
                .mail_item_text_ids
                .lock()
                .unwrap()
                .push((id, item.item_text_id));
        }
    }

    pub(crate) fn attached_text_id(&self, mail_id: u64) -> u32 {
        self.mail
            .mail_item_text_ids
            .lock()
            .unwrap()
            .iter()
            .find(|(id, _)| *id == mail_id)
            .map_or(0, |(_, text_id)| *text_id)
    }

    pub(crate) fn take_attached_text_id(&self, mail_id: u64) -> u32 {
        let mut ids = self.mail.mail_item_text_ids.lock().unwrap();
        let Some(at) = ids.iter().position(|(id, _)| *id == mail_id) else {
            return 0;
        };
        ids.remove(at).1
    }

    pub(crate) fn saw_same_account(&self, call: &'static str, same_account: bool) {
        self.mail
            .same_account_seen
            .lock()
            .unwrap()
            .push((call, same_account));
    }
}
