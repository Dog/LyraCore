use super::super::*;

#[derive(Default)]
pub(crate) struct CharacterState {
    pub(crate) character_read_error: Option<String>,
    pub(crate) deleted: std::sync::Mutex<Vec<(u64, u64)>>,
    pub(crate) delete_outcome: Option<codec::CharDeleteOutcome>,
    pub(crate) created_characters: std::sync::Mutex<Vec<codec::CharacterView>>,
    pub(crate) next_created_guid: std::sync::atomic::AtomicU64,
    pub(crate) reputations: Vec<(i32, i32, bool)>,
    pub(crate) player_actions: Vec<(u8, u32, u8)>,
    pub(crate) learned_spells: Vec<u32>,
    pub(crate) durable_absence_checks: std::sync::atomic::AtomicUsize,
}

impl CharacterStore for WorldFake {
    fn characters(&self, _account_id: u64) -> Result<Vec<codec::CharacterView>> {
        self.rec("characters");
        let mut out = self.characters.clone();
        out.extend(
            self.character
                .created_characters
                .lock()
                .unwrap()
                .iter()
                .cloned(),
        );
        Ok(out)
    }

    fn create_character(
        &self,
        _account_id: u64,
        name: &str,
        race: u8,
        class: u8,
        _gender: u8,
        _appearance: codec::Appearance,
    ) -> Result<codec::CharCreateOutcome> {
        if self.characters.iter().any(|c| c.name == name)
            || self
                .character
                .created_characters
                .lock()
                .unwrap()
                .iter()
                .any(|c| c.name == name)
        {
            return Ok(codec::CharCreateOutcome::NameInUse);
        }
        let guid = 500
            + self
                .character
                .next_created_guid
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.character
            .created_characters
            .lock()
            .unwrap()
            .push(codec::CharacterView {
                guid,
                name: name.to_string(),
                race,
                class,
                level: 1,
                ..Default::default()
            });
        Ok(codec::CharCreateOutcome::Success)
    }

    fn delete_character(
        &self,
        account_id: u64,
        character: Actor,
    ) -> Result<codec::CharDeleteOutcome> {
        self.character
            .deleted
            .lock()
            .unwrap()
            .push((account_id, character.guid()));
        Ok(self
            .character
            .delete_outcome
            .unwrap_or(codec::CharDeleteOutcome::Success))
    }

    fn character_by_guid(&self, guid: u64) -> Result<Option<codec::CharacterView>> {
        if let Some(error) = &self.character.character_read_error {
            return Err(anyhow!(error.clone()));
        }
        Ok(self.characters.iter().find(|c| c.guid == guid).cloned())
    }

    fn character_exists_on_any_world_shard(&self, guid: u64) -> Result<bool> {
        self.character
            .durable_absence_checks
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(error) = &self.topology.world_shard_set_error {
            return Err(anyhow!(error.clone()));
        }
        if self.character_by_guid(guid)?.is_some() {
            return Ok(true);
        }
        for shard in self.topology.peers.lock().unwrap().iter() {
            if shard.character_by_guid(guid)?.is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn player_skills(&self, _character_guid: u64) -> Result<Vec<(u32, u16, u16)>> {
        Ok(Vec::new())
    }

    fn effective_armor(&self, _guid: u64) -> u32 {
        self.session
            .login_entity
            .as_ref()
            .map(|e| e.effective_armor)
            .unwrap_or(0)
    }

    fn effective_magic_resistances(&self, _guid: u64) -> [u32; 6] {
        [0; 6]
    }

    fn spell_modifiers(&self, _character_guid: u64) -> Vec<(u32, u8, i32, bool)> {
        Vec::new()
    }

    fn player_learned_spells(&self, _player_guid: u64) -> Result<Vec<u32>> {
        Ok(self.character.learned_spells.clone())
    }

    fn player_reputations(&self, _player_guid: u64) -> Result<Vec<(i32, i32, bool)>> {
        Ok(self.character.reputations.clone())
    }

    fn player_actions(&self, _player_guid: u64) -> Result<Vec<(u8, u32, u8)>> {
        Ok(self.character.player_actions.clone())
    }
}
