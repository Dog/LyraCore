use anyhow::Result;

use super::chat::chat_outcome;
use crate::stdb::bindings::*;
use crate::stdb::connection::call_reducer;
use crate::stdb::Coordinator;
use crate::world::{Actor, ChatOutcome, SpeechStore};

impl SpeechStore for Coordinator {
    fn send_chat(
        &self,
        actor: Actor,
        chat_type: u8,
        language: u8,
        message: String,
    ) -> Result<ChatOutcome> {
        chat_outcome(call_reducer!(
            self.0.call_pipe().conn.reducers,
            "gw_send_chat",
            gw_send_chat_then(self.session_actor(actor), chat_type, language, message)
        ))
    }

    fn send_emote(
        &self,
        actor: Actor,
        text_emote: u32,
        emote_anim: u32,
        target_guid: u64,
    ) -> Result<()> {
        call_reducer!(
            self.0.call_pipe().conn.reducers,
            "gw_send_emote",
            gw_send_emote_then(
                self.session_actor(actor),
                text_emote,
                emote_anim,
                target_guid
            )
        )
    }

    fn gm_command(&self, account_name: &str, actor: Actor, text: String) -> Result<()> {
        crate::realm_core::run_gm_command(self, account_name, actor, text)
    }
}
