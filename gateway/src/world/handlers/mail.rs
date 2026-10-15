use super::super::*;
use wow_world_messages::vanilla::SMSG_SEND_MAIL_RESULT_MailResultTwo;

pub(crate) struct Mail;

impl<St: CharacterStore + MailStore + SessionStore + ShardRoutingStore + SocialStore + ?Sized>
    ProtocolFamily<St> for Mail
{
    #[allow(clippy::too_many_lines)]
    fn handle(
        store: &St,
        conn: &mut ProtocolSession,
        request: ProtocolRequest,
    ) -> Result<ProtocolReply> {
        let msg = request.message()?;
        let mut outbound = Vec::new();
        match msg {
            // Vanilla opens type-19 mailboxes itself; there is no SMSG_SHOW_MAILBOX.
            ClientOpcodeMessage::CMSG_GET_MAIL_LIST(c) => {
                match mail::open_mailbox(store, conn.actor(), c.mailbox.guid()) {
                    Ok(mails) => {
                        outbound.push(Outbound::One(ServerOpcodeMessage::SMSG_MAIL_LIST_RESULT(
                            Box::new(codec::build_mail_list(&mails, mail::now_secs())),
                        )))
                    }
                    Err(failure) => {
                        let refusal = failure.refusal()?;
                        log::debug!(
                            "world: mail list refused (account {}): {refusal}",
                            conn.account_id
                        );
                    }
                }
            }
            ClientOpcodeMessage::MSG_QUERY_NEXT_MAIL_TIME => {
                let unread = match mail::has_unread(store, conn.actor()) {
                    Ok(unread) => unread,
                    Err(failure) => {
                        let refusal = failure.refusal()?;
                        log::debug!(
                            "world: mail poll answered empty (account {}): {refusal}",
                            conn.account_id
                        );
                        false
                    }
                };
                outbound.push(Outbound::One(
                    ServerOpcodeMessage::MSG_QUERY_NEXT_MAIL_TIME(codec::build_next_mail_time(
                        unread,
                    )),
                ));
            }
            ClientOpcodeMessage::CMSG_ITEM_TEXT_QUERY(c) => {
                let body = match mail::item_text(
                    store,
                    conn.actor(),
                    c.item_text_id,
                    u64::from(c.mail_id),
                ) {
                    Ok(body) => body,
                    Err(failure) => {
                        let refusal = failure.refusal()?;
                        log::debug!(
                            "world: item text query refused (account {}): {refusal}",
                            conn.account_id
                        );
                        None
                    }
                };
                outbound.push(Outbound::One(
                    ServerOpcodeMessage::SMSG_ITEM_TEXT_QUERY_RESPONSE(Box::new(
                        codec::build_item_text_response(c.item_text_id, body.unwrap_or_default()),
                    )),
                ));
            }
            ClientOpcodeMessage::CMSG_MAIL_MARK_AS_READ(c) => {
                let actor = conn.actor();
                if let Err(failure) =
                    mail::mark_read(store, actor, c.mailbox.guid(), u64::from(c.mail_id))
                {
                    let refusal = failure.refusal()?;
                    log::debug!(
                        "world: mail mark-as-read refused (account {}): {refusal}",
                        conn.account_id
                    );
                }
            }
            ClientOpcodeMessage::CMSG_MAIL_DELETE(c) => {
                let actor = conn.actor();
                let ok = match mail::delete(store, actor, c.mailbox_id.guid(), u64::from(c.mail_id))
                {
                    Ok(()) => true,
                    Err(failure) => {
                        let refusal = failure.refusal()?;
                        log::debug!(
                            "world: mail delete refused (account {}): {refusal}",
                            conn.account_id
                        );
                        false
                    }
                };
                outbound.push(Outbound::One(ServerOpcodeMessage::SMSG_SEND_MAIL_RESULT(
                    Box::new(codec::build_mail_delete_result(c.mail_id, ok)),
                )));
            }
            ClientOpcodeMessage::CMSG_MAIL_RETURN_TO_SENDER(c) => {
                let actor = conn.actor();
                let ok = match mail::return_to_sender(
                    store,
                    actor,
                    c.mailbox_id.guid(),
                    u64::from(c.mail_id),
                ) {
                    Ok(()) => true,
                    Err(failure) => {
                        let refusal = failure.refusal()?;
                        log::debug!(
                            "world: mail return refused (account {}): {refusal}",
                            conn.account_id
                        );
                        false
                    }
                };
                outbound.push(Outbound::One(ServerOpcodeMessage::SMSG_SEND_MAIL_RESULT(
                    Box::new(codec::build_mail_return_result(c.mail_id, ok)),
                )));
            }
            ClientOpcodeMessage::CMSG_MAIL_TAKE_MONEY(c) => {
                let actor = conn.actor();
                let ok =
                    match mail::take_money(store, actor, c.mailbox.guid(), u64::from(c.mail_id)) {
                        Ok(()) => true,
                        Err(failure) => {
                            let refusal = failure.refusal()?;
                            log::debug!(
                                "world: mail take-money refused (account {}): {refusal}",
                                conn.account_id
                            );
                            false
                        }
                    };
                outbound.push(Outbound::One(ServerOpcodeMessage::SMSG_SEND_MAIL_RESULT(
                    Box::new(codec::build_mail_take_money_result(c.mail_id, ok)),
                )));
            }
            ClientOpcodeMessage::CMSG_MAIL_TAKE_ITEM(c) => {
                let actor = conn.actor();
                let outcome =
                    match mail::take_item(store, actor, c.mailbox.guid(), u64::from(c.mail_id)) {
                        Ok(taken) => Ok(taken),
                        Err(failure) => {
                            let e = failure.refusal()?;
                            log::debug!(
                                "world: mail take-item refused (account {}): {e}",
                                conn.account_id
                            );
                            Err(match e {
                                mail::TakeItemRefusal::BagsFull(_) => {
                                    codec::MailTakeItemError::BagsFull
                                }
                                mail::TakeItemRefusal::CannotAffordCod(_) => {
                                    codec::MailTakeItemError::NotEnoughMoney
                                }
                                mail::TakeItemRefusal::Other(_) => codec::MailTakeItemError::Other,
                            })
                        }
                    };
                outbound.push(Outbound::One(ServerOpcodeMessage::SMSG_SEND_MAIL_RESULT(
                    Box::new(codec::build_mail_take_item_result(c.mail_id, outcome)),
                )));
            }
            ClientOpcodeMessage::CMSG_SEND_MAIL(c) => {
                let result2 = match mail::send(
                    store,
                    conn.actor(),
                    c.mailbox.guid(),
                    &c.receiver,
                    c.subject.clone(),
                    c.body.clone(),
                    c.money.as_int(),
                    c.cash_on_delivery_amount,
                    c.item.guid(),
                ) {
                    Ok(()) => Some(SMSG_SEND_MAIL_RESULT_MailResultTwo::Ok),
                    Err(failure) => {
                        let e = failure.refusal()?;
                        log::debug!(
                            "world: mail send refused (account {}): {e}",
                            conn.account_id
                        );
                        match e {
                            mail::SendRefusal::NoMailbox(_) => None,
                            mail::SendRefusal::RecipientNotFound(_) => {
                                Some(SMSG_SEND_MAIL_RESULT_MailResultTwo::ErrRecipientNotFound)
                            }
                            mail::SendRefusal::CannotSendToSelf => {
                                Some(SMSG_SEND_MAIL_RESULT_MailResultTwo::ErrCannotSendToSelf)
                            }
                            mail::SendRefusal::NotYourTeam => {
                                Some(SMSG_SEND_MAIL_RESULT_MailResultTwo::ErrNotYourTeam)
                            }
                            mail::SendRefusal::NotEnoughMoney(_) => {
                                Some(SMSG_SEND_MAIL_RESULT_MailResultTwo::ErrNotEnoughMoney)
                            }
                            mail::SendRefusal::AttachmentSoulbound(_) => {
                                Some(SMSG_SEND_MAIL_RESULT_MailResultTwo::ErrCantSendWrappedCod)
                            }
                            mail::SendRefusal::AttachmentInvalid(_) => {
                                Some(SMSG_SEND_MAIL_RESULT_MailResultTwo::ErrMailAttachmentInvalid)
                            }
                            mail::SendRefusal::Internal(_) => {
                                Some(SMSG_SEND_MAIL_RESULT_MailResultTwo::ErrInternalError)
                            }
                        }
                    }
                };
                if let Some(result2) = result2 {
                    outbound.push(Outbound::One(ServerOpcodeMessage::SMSG_SEND_MAIL_RESULT(
                        Box::new(codec::build_mail_send_result(result2)),
                    )));
                }
            }
            ClientOpcodeMessage::CMSG_MAIL_CREATE_TEXT_ITEM(c) => {
                let actor = conn.actor();
                let made =
                    match mail::copy_letter(store, actor, c.mailbox.guid(), u64::from(c.mail_id)) {
                        Ok(()) => Some(Ok(())),
                        Err(failure) => {
                            let e = failure.refusal()?;
                            log::debug!(
                                "world: letter copy refused (account {}): {e}",
                                conn.account_id
                            );
                            match e {
                                mail::CopyLetterRefusal::NoMailbox(_) => None,
                                mail::CopyLetterRefusal::BagsFull(_) => {
                                    Some(Err(codec::MailMadePermanentError::BagsFull))
                                }
                                mail::CopyLetterRefusal::Other(_) => {
                                    Some(Err(codec::MailMadePermanentError::Other))
                                }
                            }
                        }
                    };
                if let Some(made) = made {
                    outbound.push(Outbound::One(ServerOpcodeMessage::SMSG_SEND_MAIL_RESULT(
                        Box::new(codec::build_mail_made_permanent_result(c.mail_id, made)),
                    )));
                }
            }
            _ => return Err(anyhow!("opcode routed to the wrong Protocol Family")),
        }
        Ok(outbound.into())
    }
}
