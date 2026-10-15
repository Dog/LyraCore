use lyracore_shared::spatial::{grid_cell, grid_cell_id, BOX_HALF_SPAN};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

pub type SessionId = u64;

pub type ShardId = usize;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EntityLayer {
    WorldEntity,
    GameObject,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct CellKey {
    pub map_id: u32,
    pub instance_id: u64,
    pub cell: i64,
}

impl CellKey {
    pub fn at(map_id: u32, instance_id: u64, gx: i32, gy: i32) -> Self {
        Self {
            map_id,
            instance_id,
            cell: grid_cell_id(gx, gy),
        }
    }

    pub fn of_position(map_id: u32, instance_id: u64, x: f32, y: f32) -> Self {
        let (gx, gy) = grid_cell(x, y);
        Self::at(map_id, instance_id, gx, gy)
    }

    pub fn partition(&self) -> (u32, u64) {
        (self.map_id, self.instance_id)
    }

    pub fn neighbourhood(&self) -> impl Iterator<Item = CellKey> {
        self.neighbourhood_within(BOX_HALF_SPAN)
    }

    pub fn neighbourhood_within(&self, half_span: i32) -> impl Iterator<Item = CellKey> {
        let (gx, gy) = lyracore_shared::spatial::grid_cell_of_id(self.cell);
        let (map_id, instance_id) = (self.map_id, self.instance_id);
        ((gx - half_span)..=(gx + half_span)).flat_map(move |cx| {
            ((gy - half_span)..=(gy + half_span))
                .map(move |cy| CellKey::at(map_id, instance_id, cx, cy))
        })
    }

    #[cfg(test)]
    pub fn grid_box(&self) -> lyracore_shared::spatial::GridBox {
        let (gx, gy) = lyracore_shared::spatial::grid_cell_of_id(self.cell);
        lyracore_shared::spatial::GridBox {
            map_id: self.map_id,
            gx,
            gy,
        }
    }
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct ViewerDelta {
    pub entered: Vec<(u64, ShardId)>,
    pub left: Vec<(u64, ShardId)>,
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct RecenterDelta {
    pub world_entities: ViewerDelta,
    pub game_objects: ViewerDelta,
}

#[derive(Default)]
struct EntityIndex {
    cell_entities: HashMap<CellKey, HashSet<u64>>,
    entity_cell: HashMap<u64, (CellKey, ShardId, u64)>,
}

#[derive(Default)]
struct Inner {
    world_entities: EntityIndex,
    game_objects: EntityIndex,
    cell_viewers: HashMap<CellKey, Vec<SessionId>>,
    viewer_cell: HashMap<SessionId, CellKey>,
}

/// Keep I/O, encoding and Relays outside the index mutex; copy results before calling them.
pub struct WorldIndex {
    inner: Mutex<Inner>,
    cell_scoped: bool,
}

impl WorldIndex {
    pub fn new(cell_scoped: bool) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            cell_scoped,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| {
            log::error!(
                "world index lock poisoned (a prior panic in a critical section) — recovering"
            );
            p.into_inner()
        })
    }

    fn layer(inner: &Inner, layer: EntityLayer) -> &EntityIndex {
        match layer {
            EntityLayer::WorldEntity => &inner.world_entities,
            EntityLayer::GameObject => &inner.game_objects,
        }
    }

    fn layer_mut(inner: &mut Inner, layer: EntityLayer) -> &mut EntityIndex {
        match layer {
            EntityLayer::WorldEntity => &mut inner.world_entities,
            EntityLayer::GameObject => &mut inner.game_objects,
        }
    }

    pub fn upsert_entity(
        &self,
        layer: EntityLayer,
        guid: u64,
        key: CellKey,
        shard: ShardId,
        owner_guid: u64,
    ) -> Option<CellKey> {
        let mut inner = self.lock();
        let entities = Self::layer_mut(&mut inner, layer);
        let previous = entities
            .entity_cell
            .insert(guid, (key, shard, owner_guid))
            .map(|(k, _, _)| k);
        match previous {
            Some(old) if old == key => return Some(old),
            Some(old) => {
                if let Some(set) = entities.cell_entities.get_mut(&old) {
                    set.remove(&guid);
                    if set.is_empty() {
                        entities.cell_entities.remove(&old);
                    }
                }
            }
            None => {}
        }
        entities.cell_entities.entry(key).or_default().insert(guid);
        previous
    }

    pub fn shard_of(&self, layer: EntityLayer, guid: u64) -> Option<ShardId> {
        let inner = self.lock();
        Self::layer(&inner, layer)
            .entity_cell
            .get(&guid)
            .map(|(_, s, _)| *s)
    }

    pub fn remove_entity(
        &self,
        layer: EntityLayer,
        guid: u64,
        shard: ShardId,
    ) -> Option<(CellKey, u64)> {
        let mut inner = self.lock();
        let entities = Self::layer_mut(&mut inner, layer);
        let &(key, owner, owner_guid) = entities.entity_cell.get(&guid)?;
        if owner != shard {
            return None;
        }
        entities.entity_cell.remove(&guid);
        if let Some(set) = entities.cell_entities.get_mut(&key) {
            set.remove(&guid);
            if set.is_empty() {
                entities.cell_entities.remove(&key);
            }
        }
        Some((key, owner_guid))
    }

    #[cfg(test)]
    pub fn entity_cell(&self, layer: EntityLayer, guid: u64) -> Option<CellKey> {
        let inner = self.lock();
        Self::layer(&inner, layer)
            .entity_cell
            .get(&guid)
            .map(|(k, _, _)| *k)
    }

    pub fn entity_cell_on_shard(
        &self,
        layer: EntityLayer,
        guid: u64,
        shard: ShardId,
    ) -> Option<CellKey> {
        let inner = self.lock();
        Self::layer(&inner, layer)
            .entity_cell
            .get(&guid)
            .and_then(|(key, owner, _)| (*owner == shard).then_some(*key))
    }

    pub fn replace_shard(
        &self,
        layer: EntityLayer,
        shard: ShardId,
        rows: impl IntoIterator<Item = (u64, CellKey, u64)>,
    ) -> Vec<(u64, CellKey, u64)> {
        let mut inner = self.lock();
        let entities = Self::layer_mut(&mut inner, layer);
        let fresh: HashMap<_, _> = rows
            .into_iter()
            .filter(|(guid, _, _)| {
                entities
                    .entity_cell
                    .get(guid)
                    .is_none_or(|(_, owner, _)| *owner == shard)
            })
            .map(|(guid, key, owner)| (guid, (key, owner)))
            .collect();
        let old: Vec<_> = entities
            .entity_cell
            .iter()
            .filter(|(_, (_, owner, _))| *owner == shard)
            .map(|(guid, (key, _, owner_guid))| (*guid, *key, *owner_guid))
            .collect();
        let mut removed = Vec::new();
        for (guid, key, owner_guid) in old {
            if fresh.get(&guid).map(|(cell, _)| cell) == Some(&key) {
                continue;
            }
            entities.entity_cell.remove(&guid);
            if let Some(guids) = entities.cell_entities.get_mut(&key) {
                guids.remove(&guid);
                if guids.is_empty() {
                    entities.cell_entities.remove(&key);
                }
            }
            removed.push((guid, key, owner_guid));
        }
        for (guid, (key, owner_guid)) in fresh {
            entities.entity_cell.insert(guid, (key, shard, owner_guid));
            entities.cell_entities.entry(key).or_default().insert(guid);
        }
        removed
    }

    pub fn add_viewer(&self, session: SessionId, key: CellKey) {
        let mut inner = self.lock();
        Self::place_viewer(&mut inner, session, key);
    }

    pub fn remove_viewer(&self, session: SessionId) {
        let mut inner = self.lock();
        if let Some(old) = inner.viewer_cell.remove(&session) {
            Self::unplace_viewer(&mut inner, session, old);
        }
    }

    #[cfg(test)]
    pub fn viewer_cell(&self, session: SessionId) -> Option<CellKey> {
        self.lock().viewer_cell.get(&session).copied()
    }

    fn place_viewer(inner: &mut Inner, session: SessionId, key: CellKey) {
        if let Some(old) = inner.viewer_cell.insert(session, key) {
            if old == key {
                return;
            }
            Self::unplace_viewer(inner, session, old);
        }
        inner.cell_viewers.entry(key).or_default().push(session);
    }

    fn unplace_viewer(inner: &mut Inner, session: SessionId, key: CellKey) {
        if let Some(v) = inner.cell_viewers.get_mut(&key) {
            v.retain(|s| *s != session);
            if v.is_empty() {
                inner.cell_viewers.remove(&key);
            }
        }
    }

    pub fn viewers_of(&self, _layer: EntityLayer, key: CellKey) -> Vec<SessionId> {
        self.viewers_within(key, BOX_HALF_SPAN)
    }

    pub fn viewers_within(&self, key: CellKey, half_span: i32) -> Vec<SessionId> {
        let inner = self.lock();
        if !self.cell_scoped {
            let partition = key.partition();
            return inner
                .viewer_cell
                .iter()
                .filter(|(_, k)| k.partition() == partition)
                .map(|(s, _)| *s)
                .collect();
        }
        let mut out = Vec::new();
        for anchor in key.neighbourhood_within(half_span) {
            if let Some(sessions) = inner.cell_viewers.get(&anchor) {
                out.extend_from_slice(sessions);
            }
        }
        out
    }

    pub fn move_viewer_delta(&self, session: SessionId, key: CellKey) -> Option<RecenterDelta> {
        let mut inner = self.lock();
        let old = *inner.viewer_cell.get(&session)?;
        if old == key {
            return None;
        }
        Self::place_viewer(&mut inner, session, key);
        if !self.cell_scoped {
            return Some(RecenterDelta::default());
        }
        let old_cells: HashSet<CellKey> = old.neighbourhood().collect();
        let new_cells: HashSet<CellKey> = key.neighbourhood().collect();
        let gather = |entities: &EntityIndex,
                      cells: &HashSet<CellKey>,
                      other: &HashSet<CellKey>|
         -> Vec<(u64, ShardId)> {
            let mut out = Vec::new();
            for cell in cells.difference(other) {
                if let Some(guids) = entities.cell_entities.get(cell) {
                    for guid in guids {
                        let shard = entities
                            .entity_cell
                            .get(guid)
                            .map(|(_, s, _)| *s)
                            .unwrap_or(0);
                        out.push((*guid, shard));
                    }
                }
            }
            out
        };
        let delta = |entities: &EntityIndex| ViewerDelta {
            entered: gather(entities, &new_cells, &old_cells),
            left: gather(entities, &old_cells, &new_cells),
        };
        Some(RecenterDelta {
            world_entities: delta(&inner.world_entities),
            game_objects: delta(&inner.game_objects),
        })
    }

    pub fn visible_entities(&self, layer: EntityLayer, session: SessionId) -> Vec<(u64, ShardId)> {
        let inner = self.lock();
        let Some(anchor) = inner.viewer_cell.get(&session).copied() else {
            return Vec::new();
        };
        let entities = Self::layer(&inner, layer);
        if !self.cell_scoped {
            let partition = anchor.partition();
            return entities
                .entity_cell
                .iter()
                .filter(|(_, (k, _, _))| k.partition() == partition)
                .map(|(guid, (_, shard, _))| (*guid, *shard))
                .collect();
        }
        let mut out = Vec::new();
        for cell in anchor.neighbourhood() {
            if let Some(guids) = entities.cell_entities.get(&cell) {
                for guid in guids {
                    if let Some((_, shard, _)) = entities.entity_cell.get(guid) {
                        out.push((*guid, *shard));
                    }
                }
            }
        }
        out
    }

    pub fn can_see(&self, layer: EntityLayer, session: SessionId, guid: u64) -> bool {
        let inner = self.lock();
        let Some(anchor) = inner.viewer_cell.get(&session) else {
            return false;
        };
        let Some((cell, _, _)) = Self::layer(&inner, layer).entity_cell.get(&guid) else {
            return false;
        };
        if anchor.partition() != cell.partition() {
            return false;
        }
        if !self.cell_scoped {
            return true;
        }
        let (ax, ay) = lyracore_shared::spatial::grid_cell_of_id(anchor.cell);
        let (cx, cy) = lyracore_shared::spatial::grid_cell_of_id(cell.cell);
        (ax - cx).abs() <= BOX_HALF_SPAN && (ay - cy).abs() <= BOX_HALF_SPAN
    }

    #[cfg(test)]
    pub fn stats(&self, layer: EntityLayer) -> (usize, usize, usize) {
        let inner = self.lock();
        let entities = Self::layer(&inner, layer);
        (
            entities.entity_cell.len(),
            inner.viewer_cell.len(),
            entities.cell_entities.len(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stdb::aoi::box_queries;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Row {
        guid: u64,
        map_id: u32,
        instance_id: u64,
        gx: i32,
        gy: i32,
    }

    fn sql_selects(query: &str, row: &Row) -> bool {
        let (_, where_clause) = query
            .split_once(" WHERE ")
            .expect("an AOI query has a WHERE");
        where_clause.split(" AND ").all(|term| {
            let mut parts = term.split_whitespace();
            let col = parts.next().expect("a column");
            let op = parts.next().expect("an operator");
            let literal: i64 = parts
                .next()
                .expect("a literal")
                .parse()
                .expect("an integer literal");
            assert!(
                parts.next().is_none(),
                "unexpected trailing token in `{term}`"
            );
            let actual: i64 = match col {
                "map_id" => row.map_id as i64,
                "instance_id" => row.instance_id as i64,
                "grid_x" => row.gx as i64,
                "grid_y" => row.gy as i64,
                "cell" => grid_cell_id(row.gx, row.gy),
                other => {
                    panic!("the oracle does not know column `{other}` — teach it or fix the query")
                }
            };
            match op {
                "=" => actual == literal,
                ">=" => actual >= literal,
                "<=" => actual <= literal,
                other => panic!("the oracle does not know operator `{other}`"),
            }
        })
    }

    fn sql_visible(layer: EntityLayer, viewer: CellKey, rows: &[Row]) -> HashSet<u64> {
        let queries = box_queries(viewer.instance_id, &viewer.grid_box(), false);
        let table = match layer {
            EntityLayer::WorldEntity => "game_world_entity",
            EntityLayer::GameObject => "game_gameobject",
        };
        let entity_query = queries
            .iter()
            .find(|q| q.starts_with(&format!("SELECT * FROM {table} ")))
            .unwrap_or_else(|| panic!("the box query set must scope {table}"));
        rows.iter()
            .filter(|r| sql_selects(entity_query, r))
            .map(|r| r.guid)
            .collect()
    }

    fn index_visible(index: &WorldIndex, layer: EntityLayer, session: SessionId) -> HashSet<u64> {
        index
            .visible_entities(layer, session)
            .into_iter()
            .map(|(guid, _)| guid)
            .collect()
    }

    fn build(layer: EntityLayer, rows: &[Row]) -> WorldIndex {
        let index = WorldIndex::new(true);
        for r in rows {
            index.upsert_entity(
                layer,
                r.guid,
                CellKey::at(r.map_id, r.instance_id, r.gx, r.gy),
                0,
                0,
            );
        }
        index
    }

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn range(&mut self, lo: i32, hi: i32) -> i32 {
            lo + (self.next() % ((hi - lo + 1) as u64)) as i32
        }
    }

    #[test]
    fn the_index_visible_set_equals_the_box_subscription_sql_for_every_viewer() {
        let mut rng = Rng(0x5EED_1234_ABCD_0001);
        for trial in 0..200 {
            let rows: Vec<Row> = (0..60)
                .map(|i| Row {
                    guid: 1000 + i,
                    map_id: rng.range(0, 1) as u32,
                    instance_id: rng.range(0, 2) as u64,
                    gx: rng.range(-4, 4),
                    gy: rng.range(-4, 4),
                })
                .collect();
            for layer in [EntityLayer::WorldEntity, EntityLayer::GameObject] {
                let index = build(layer, &rows);
                for v in 0..12u64 {
                    let viewer = CellKey::at(
                        rng.range(0, 1) as u32,
                        rng.range(0, 2) as u64,
                        rng.range(-4, 4),
                        rng.range(-4, 4),
                    );
                    index.add_viewer(v, viewer);
                    assert_eq!(
                        index_visible(&index, layer, v),
                        sql_visible(layer, viewer, &rows),
                        "trial {trial}, layer {layer:?}, viewer {v} anchored at {viewer:?}: the \
                         index and the box subscription SQL disagree about who is visible"
                    );
                    index.remove_viewer(v);
                }
            }
        }
    }

    #[test]
    fn viewers_of_a_cell_is_exactly_the_inverse_of_the_box_subscription_sql() {
        let mut rng = Rng(0x00C0_FFEE_0000_0042);
        for _ in 0..200 {
            let viewers: Vec<(SessionId, CellKey)> = (0..16u64)
                .map(|v| {
                    (
                        v,
                        CellKey::at(
                            rng.range(0, 1) as u32,
                            rng.range(0, 1) as u64,
                            rng.range(-3, 3),
                            rng.range(-3, 3),
                        ),
                    )
                })
                .collect();
            let index = WorldIndex::new(true);
            for (v, key) in &viewers {
                index.add_viewer(*v, *key);
            }
            for _ in 0..10 {
                let row = Row {
                    guid: 7,
                    map_id: rng.range(0, 1) as u32,
                    instance_id: rng.range(0, 1) as u64,
                    gx: rng.range(-5, 5),
                    gy: rng.range(-5, 5),
                };
                let cell = CellKey::at(row.map_id, row.instance_id, row.gx, row.gy);
                let from_index: HashSet<SessionId> = index
                    .viewers_of(EntityLayer::WorldEntity, cell)
                    .into_iter()
                    .collect();
                let from_sql: HashSet<SessionId> = viewers
                    .iter()
                    .filter(|(_, anchor)| {
                        !sql_visible(EntityLayer::WorldEntity, *anchor, &[row]).is_empty()
                    })
                    .map(|(v, _)| *v)
                    .collect();
                assert_eq!(
                    from_index, from_sql,
                    "row {row:?} — the dispatch's recipient set disagrees with the SQL that used \
                     to deliver it"
                );
            }
        }
    }

    #[test]
    fn a_recenter_delta_is_exactly_the_difference_between_the_two_sql_visible_sets() {
        let mut rng = Rng(0xDEAD_BEEF_0000_0007);
        for _ in 0..300 {
            let rows: Vec<Row> = (0..50)
                .map(|i| Row {
                    guid: 2000 + i,
                    map_id: 0,
                    instance_id: 0,
                    gx: rng.range(-6, 6),
                    gy: rng.range(-6, 6),
                })
                .collect();
            let index = WorldIndex::new(true);
            for layer in [EntityLayer::WorldEntity, EntityLayer::GameObject] {
                for row in &rows {
                    index.upsert_entity(
                        layer,
                        row.guid,
                        CellKey::at(row.map_id, row.instance_id, row.gx, row.gy),
                        0,
                        0,
                    );
                }
            }
            let from = CellKey::at(0, 0, rng.range(-4, 4), rng.range(-4, 4));
            let to = CellKey::at(0, 0, rng.range(-4, 4), rng.range(-4, 4));
            index.add_viewer(1, from);
            let before = sql_visible(EntityLayer::WorldEntity, from, &rows);
            let delta = index.move_viewer_delta(1, to);
            let after = sql_visible(EntityLayer::WorldEntity, to, &rows);
            match delta {
                None => assert_eq!(
                    from, to,
                    "a delta of None must mean the anchor did not move"
                ),
                Some(d) => {
                    for (layer, layer_delta) in [
                        (EntityLayer::WorldEntity, d.world_entities),
                        (EntityLayer::GameObject, d.game_objects),
                    ] {
                        let entered: HashSet<u64> =
                            layer_delta.entered.iter().map(|(g, _)| *g).collect();
                        let left: HashSet<u64> = layer_delta.left.iter().map(|(g, _)| *g).collect();
                        assert_eq!(
                            entered,
                            after.difference(&before).copied().collect::<HashSet<u64>>(),
                            "{layer:?} entered set wrong moving {from:?} -> {to:?}"
                        );
                        assert_eq!(
                            left,
                            before.difference(&after).copied().collect::<HashSet<u64>>(),
                            "{layer:?} left set wrong moving {from:?} -> {to:?}"
                        );
                        assert_eq!(index_visible(&index, layer, 1), after);
                    }
                }
            }
            index.remove_viewer(1);
        }
    }

    #[test]
    fn neighbourhood_is_exactly_the_grid_box_bounds() {
        let key = CellKey::at(3, 9, 12, -7);
        let (x0, x1, y0, y1) = key.grid_box().bounds();
        let got: HashSet<(i32, i32)> = key
            .neighbourhood()
            .map(|k| lyracore_shared::spatial::grid_cell_of_id(k.cell))
            .collect();
        let want: HashSet<(i32, i32)> = (x0..=x1)
            .flat_map(|x| (y0..=y1).map(move |y| (x, y)))
            .collect();
        assert_eq!(got, want);
        assert_eq!(got.len(), ((2 * BOX_HALF_SPAN + 1) as usize).pow(2));
        assert!(key
            .neighbourhood()
            .all(|k| k.map_id == 3 && k.instance_id == 9));
    }

    #[test]
    fn viewers_within_widens_the_box_by_chebyshev_distance() {
        let index = WorldIndex::new(true);
        let centre = CellKey::at(0, 0, 0, 0);
        let mut expected: HashMap<i32, HashSet<SessionId>> = HashMap::new();
        let mut session = 0;
        for gx in -8..=8 {
            for gy in -8..=8 {
                session += 1;
                index.add_viewer(session, CellKey::at(0, 0, gx, gy));
                for span in 0..=8 {
                    if gx.abs() <= span && gy.abs() <= span {
                        expected.entry(span).or_default().insert(session);
                    }
                }
            }
        }
        index.add_viewer(session + 1, CellKey::at(0, 1, 0, 0));
        for span in 0..=8 {
            let got: HashSet<SessionId> = index.viewers_within(centre, span).into_iter().collect();
            assert_eq!(got, expected[&span], "span {span}");
        }
        assert_eq!(
            index.viewers_within(centre, BOX_HALF_SPAN),
            index.viewers_of(EntityLayer::WorldEntity, centre)
        );
    }

    #[test]
    fn an_entity_move_leaves_no_residue_in_its_old_cell() {
        let index = WorldIndex::new(true);
        let a = CellKey::at(0, 0, 0, 0);
        let b = CellKey::at(0, 0, 40, 40);
        index.upsert_entity(EntityLayer::WorldEntity, 5, a, 0, 0);
        index.add_viewer(1, a);
        assert_eq!(
            index_visible(&index, EntityLayer::WorldEntity, 1),
            HashSet::from([5])
        );
        assert_eq!(
            index.upsert_entity(EntityLayer::WorldEntity, 5, b, 0, 0),
            Some(a)
        );
        assert!(
            index_visible(&index, EntityLayer::WorldEntity, 1).is_empty(),
            "stale cell membership"
        );
        assert_eq!(index.entity_cell(EntityLayer::WorldEntity, 5), Some(b));
        assert_eq!(
            index.remove_entity(EntityLayer::WorldEntity, 5, 0),
            Some((b, 0))
        );
        assert_eq!(index.entity_cell(EntityLayer::WorldEntity, 5), None);
        assert_eq!(
            index.remove_entity(EntityLayer::WorldEntity, 5, 0),
            None,
            "removal is idempotent"
        );
    }

    #[test]
    fn a_shard_snapshot_replaces_its_rows_without_erasing_viewers_or_a_transfer_destination() {
        let index = WorldIndex::new(true);
        let near = CellKey::at(0, 0, 0, 0);
        let far = CellKey::at(0, 0, 20, 20);
        index.add_viewer(1, near);
        index.upsert_entity(EntityLayer::WorldEntity, 10, near, 0, 0);
        index.upsert_entity(EntityLayer::WorldEntity, 20, near, 0, 0);
        index.upsert_entity(EntityLayer::WorldEntity, 30, near, 1, 0);

        let removed = index.replace_shard(
            EntityLayer::WorldEntity,
            0,
            [(20, far, 0), (30, far, 0), (40, near, 0)],
        );
        assert_eq!(
            removed.into_iter().collect::<HashSet<_>>(),
            HashSet::from([(10, near, 0), (20, near, 0)])
        );
        assert_eq!(index.viewer_cell(1), Some(near));
        assert_eq!(index.shard_of(EntityLayer::WorldEntity, 30), Some(1));
        assert_eq!(index.entity_cell(EntityLayer::WorldEntity, 30), Some(near));
        assert_eq!(
            index_visible(&index, EntityLayer::WorldEntity, 1),
            HashSet::from([30, 40])
        );
        assert_eq!(index.entity_cell(EntityLayer::WorldEntity, 20), Some(far));
    }

    #[test]
    fn a_viewer_recenter_is_a_cell_swap_and_reports_only_real_crossings() {
        let index = WorldIndex::new(true);
        let a = CellKey::at(0, 0, 0, 0);
        index.add_viewer(1, a);
        assert!(
            index.move_viewer_delta(1, a).is_none(),
            "same cell is not a recenter"
        );
        assert!(index
            .move_viewer_delta(1, CellKey::at(0, 0, 1, 0))
            .is_some());
        assert_eq!(index.viewer_cell(1), Some(CellKey::at(0, 0, 1, 0)));
        index.upsert_entity(EntityLayer::WorldEntity, 5, CellKey::at(0, 0, 0, 0), 0, 0);
        index.move_viewer_delta(1, CellKey::at(0, 0, 30, 30));
        assert!(index
            .viewers_of(EntityLayer::WorldEntity, CellKey::at(0, 0, 0, 0))
            .is_empty());
        assert!(
            index.move_viewer_delta(2, a).is_none(),
            "an unregistered session never moves"
        );
        index.remove_viewer(1);
        assert_eq!(index.viewer_cell(1), None);
        index.remove_viewer(1);
    }

    #[test]
    fn the_aoi_off_hatch_sees_the_whole_partition_and_nothing_outside_it() {
        let index = WorldIndex::new(false);
        index.upsert_entity(EntityLayer::WorldEntity, 1, CellKey::at(0, 0, 0, 0), 0, 0);
        index.upsert_entity(
            EntityLayer::WorldEntity,
            2,
            CellKey::at(0, 0, 500, 500),
            0,
            0,
        );
        index.upsert_entity(EntityLayer::WorldEntity, 3, CellKey::at(0, 77, 0, 0), 0, 0);
        index.upsert_entity(EntityLayer::WorldEntity, 4, CellKey::at(1, 0, 0, 0), 0, 0);
        index.add_viewer(1, CellKey::at(0, 0, 0, 0));
        assert_eq!(
            index_visible(&index, EntityLayer::WorldEntity, 1),
            HashSet::from([1, 2])
        );
        assert_eq!(
            index.viewers_of(EntityLayer::WorldEntity, CellKey::at(0, 0, 900, 900)),
            vec![1]
        );
        assert!(index
            .viewers_of(EntityLayer::WorldEntity, CellKey::at(0, 77, 0, 0))
            .is_empty());
    }

    #[test]
    fn can_see_agrees_with_visible_entities_everywhere() {
        let mut rng = Rng(0x1234_5678_9ABC_DEF1);
        let rows: Vec<Row> = (0..80)
            .map(|i| Row {
                guid: 3000 + i,
                map_id: rng.range(0, 1) as u32,
                instance_id: rng.range(0, 1) as u64,
                gx: rng.range(-6, 6),
                gy: rng.range(-6, 6),
            })
            .collect();
        let index = build(EntityLayer::WorldEntity, &rows);
        for _ in 0..50 {
            let anchor = CellKey::at(
                rng.range(0, 1) as u32,
                rng.range(0, 1) as u64,
                rng.range(-5, 5),
                rng.range(-5, 5),
            );
            index.add_viewer(1, anchor);
            let visible = index_visible(&index, EntityLayer::WorldEntity, 1);
            for r in &rows {
                assert_eq!(
                    index.can_see(EntityLayer::WorldEntity, 1, r.guid),
                    visible.contains(&r.guid),
                    "anchor {anchor:?} row {r:?}"
                );
            }
            assert!(
                !index.can_see(EntityLayer::WorldEntity, 1, 1),
                "an unknown guid is never visible"
            );
            assert!(
                !index.can_see(EntityLayer::WorldEntity, 99, rows[0].guid),
                "an unknown session sees nothing"
            );
            index.remove_viewer(1);
        }
    }

    #[test]
    fn stats_count_entities_viewers_and_occupied_cells() {
        let index = WorldIndex::new(true);
        index.upsert_entity(EntityLayer::WorldEntity, 1, CellKey::at(0, 0, 0, 0), 0, 0);
        index.upsert_entity(EntityLayer::WorldEntity, 2, CellKey::at(0, 0, 0, 0), 0, 0);
        index.upsert_entity(EntityLayer::WorldEntity, 3, CellKey::at(0, 0, 1, 0), 0, 0);
        index.add_viewer(1, CellKey::at(0, 0, 0, 0));
        assert_eq!(index.stats(EntityLayer::WorldEntity), (3, 1, 2));
    }
}
