use anyhow::Result;

use super::super::bindings::*;
use super::super::connection::Coordinator;

impl Coordinator {
    pub fn gameobject_template(
        &self,
        entry: u32,
    ) -> Result<Option<crate::codec::GameObjectTemplateView>> {
        let guard = self.0.coord();
        let db = &guard.conn.db;
        Ok(db.game_gameobject_template().entry().find(&entry).map(|t| {
            let stone = (t.type_id == lyracore_shared::constants::go_type::MEETINGSTONE)
                .then(|| db.game_meeting_stone().entry().find(&entry))
                .flatten();
            let (data0, data1, data2) = match stone {
                Some(stone) => (stone.min_level, stone.max_level, stone.area_id),
                None => (t.data_0, t.data_1, 0),
            };
            crate::codec::GameObjectTemplateView {
                type_id: t.type_id,
                display_id: t.display_id,
                name: t.name,
                data0,
                data1,
                data2,
            }
        }))
    }
}
