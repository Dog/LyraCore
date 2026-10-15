use super::super::*;

#[derive(Default)]
pub(crate) struct SpeechState {
    pub(crate) send_chat_outcome: Option<ChatOutcome>,
    pub(crate) send_chat_error: Option<fn() -> anyhow::Error>,
    pub(crate) gm_command_error: Option<fn() -> anyhow::Error>,
    pub(crate) gm_commands: std::sync::Mutex<Vec<(String, String)>>,
    pub(crate) gm_alpha_test_tools: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    pub(crate) gm_authority_results: std::sync::Mutex<Vec<bool>>,
    pub(crate) gm_gameplay_changes: std::sync::Mutex<Vec<String>>,
    pub(crate) chats: std::sync::Mutex<Vec<(u8, u8, String)>>,
    pub(crate) emotes: std::sync::Mutex<Vec<(u32, u32, u64)>>,
}

impl SpeechStore for WorldFake {
    fn send_chat(
        &self,
        _actor: Actor,
        chat_type: u8,
        language: u8,
        message: String,
    ) -> Result<ChatOutcome> {
        if let Some(error) = self.speech.send_chat_error {
            return Err(error());
        }
        self.rec("send_chat");
        self.speech
            .chats
            .lock()
            .unwrap()
            .push((chat_type, language, message));
        Ok(self
            .speech
            .send_chat_outcome
            .unwrap_or(ChatOutcome::Delivered))
    }

    fn send_emote(
        &self,
        _actor: Actor,
        text_emote: u32,
        emote_anim: u32,
        target_guid: u64,
    ) -> Result<()> {
        self.rec("send_emote");
        self.speech
            .emotes
            .lock()
            .unwrap()
            .push((text_emote, emote_anim, target_guid));
        Ok(())
    }

    fn gm_command(&self, account_name: &str, _actor: Actor, text: String) -> Result<()> {
        if let Some(alpha_test_tools) = &self.speech.gm_alpha_test_tools {
            let authorized = alpha_test_tools.load(std::sync::atomic::Ordering::SeqCst);
            self.speech
                .gm_commands
                .lock()
                .unwrap()
                .push((account_name.to_string(), text.clone()));
            self.speech
                .gm_authority_results
                .lock()
                .unwrap()
                .push(authorized);
            if authorized && (text.starts_with(".speed") || text.starts_with(".tele")) {
                self.speech.gm_gameplay_changes.lock().unwrap().push(text);
                return Ok(());
            }
            return Err(crate::stdb::ReducerCallError::refused(
                "gw_gm_command",
                "permission denied",
            )
            .into());
        }
        match self.speech.gm_command_error {
            Some(error) => Err(error()),
            None => {
                self.speech
                    .gm_commands
                    .lock()
                    .unwrap()
                    .push((account_name.to_string(), text));
                Ok(())
            }
        }
    }
}
