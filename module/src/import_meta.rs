//! Import provenance stamps. `game_import_meta` records, per FAMILY, the source
//! SHA / file hash / row count / timestamp of the last successful (re)load, so a live DB can answer
//! "what data am I running?" via `spacetime sql` — a question neither `import-world.sh`'s floor
//! assertions nor the wire suite's data gates (docs/danger-zones.md) can answer on their own (they
//! prove data LANDED, not WHERE it came from).
//!
//! NOT public — the precedent for genuinely non-`public` module tables is `game_taunt_lock`
//! (threat.rs) / `game_event_reaper_schedule` (gc.rs). (`game_lock`/`game_creature_family` are a
//! DIFFERENT axis: `public` but gateway-UNSUBSCRIBED — visible to any SpacetimeDB client, just
//! never bound/read by our gateway.) Nothing subscribes this table, so it needs no gateway binding
//! regen — docs/danger-zones.md §1.2's "new table → regenerate the gateway bindings" rule applies
//! to tables the gateway actually reads. The importer and an operator via `spacetime sql` still
//! read/write it directly against the SpacetimeDB node.
//!
//! `game_world_import` records which maps a completed World Import loaded on this database. The CLI's
//! `import world` brackets each destination with `begin_world_import` and `complete_world_import`;
//! `dev check`, `import check` and `dev smoke` ask `check_world_content` before play.
use spacetimedb::{reducer, table, ReducerContext, Table, Timestamp};

use crate::helpers::require_operator;

/// Maps that need imported WMO collision before play. The importer's `--vmap` mode has an
/// instance slice for Deadmines only; the Deeprun Tram (369) joins once it has one.
const WMO_MAPS: [u32; 1] = [36];

/// One row per FAMILY (a `--dump`/`--dbc` import family name — e.g. "creatures", "quests" — or a
/// `debug_seed_<name>` reseed) recording the provenance of its last successful load. `family` is the
/// primary key: a re-import UPSERTS (delete+insert) rather than appending history, so a query always
/// shows the CURRENT state — "what data am I running right now", not a full audit log.
#[table(accessor = game_import_meta)]
pub struct ImportMeta {
    #[primary_key]
    pub family: String,
    /// The resolved cmangos `classic-db` commit SHA this load ran against. Empty string when the
    /// caller has no external dump provenance (e.g. a `debug_seed_*` reseed, or a `--dbc`-only
    /// family that reads client DBCs rather than the cmangos dump).
    pub source_sha: String,
    /// sha256 hex digest of the dump bytes actually parsed for this run. Empty string when not
    /// applicable (same cases as `source_sha`).
    pub file_hash: String,
    /// The row count this family's load actually wrote/touched — family-scoped, not a global table
    /// size, so it stays meaningful across families that share an underlying table.
    pub row_count: u64,
    pub imported_at: Timestamp,
}

/// Upsert `family`'s provenance row. The shared core both `stamp_import_meta` (below, the importer's
/// operator-gated entry point) and module-side callers (`debug.rs`'s `debug_seed_*` reseed reducers,
/// which run under the `debug_reducers` feature and don't go through the operator gate themselves)
/// use. Delete-then-insert — the same upsert convention `debug_compute_swing`'s
/// `readouts.key().delete(&key)` already uses for a primary-keyed "latest state" row — rather than an
/// append-only history, so a re-run overwrites the prior stamp for that family.
pub(crate) fn stamp(
    ctx: &ReducerContext,
    family: &str,
    source_sha: &str,
    file_hash: &str,
    row_count: u64,
) {
    let table = ctx.db.game_import_meta();
    table.family().delete(family.to_string());
    table.insert(ImportMeta {
        family: family.to_string(),
        source_sha: source_sha.to_string(),
        file_hash: file_hash.to_string(),
        row_count,
        imported_at: ctx.timestamp,
    });
}

/// Operator-only entry point the importer (a privileged, non-gateway SpacetimeDB client — see
/// `require_operator`'s doc on why this check can't move to the gateway) calls after each family's
/// successful clear+reload to record its provenance. NOT feature-gated (unlike `debug.rs`'s harness
/// reducers behind `debug_reducers`): this is production tooling the importer calls on every real
/// run, not a test-only actuator.
#[reducer]
pub fn stamp_import_meta(
    ctx: &ReducerContext,
    family: String,
    source_sha: String,
    file_hash: String,
    row_count: u64,
) -> Result<(), String> {
    require_operator(ctx)?;
    stamp(ctx, &family, &source_sha, &file_hash, row_count);
    Ok(())
}

/// One row per map whose World Import completed on this database.
#[table(accessor = game_world_import)]
pub struct WorldImport {
    #[primary_key]
    pub map_id: u32,
    pub completed_at: Timestamp,
}

/// Clear every completion on this database before an import reloads it.
#[reducer]
pub fn begin_world_import(ctx: &ReducerContext) -> Result<(), String> {
    require_operator(ctx)?;
    let table = ctx.db.game_world_import();
    let maps: Vec<u32> = table.iter().map(|row| row.map_id).collect();
    for map_id in maps {
        table.map_id().delete(map_id);
    }
    Ok(())
}

/// Record `map_ids` as imported, after their content passes the same checks as play.
#[reducer]
pub fn complete_world_import(ctx: &ReducerContext, map_ids: Vec<u32>) -> Result<(), String> {
    require_operator(ctx)?;
    content_problem(
        &map_ids,
        |_| true,
        |map| crate::vmap::active_generation_id(ctx, map).is_some(),
    )?;
    let table = ctx.db.game_world_import();
    for map_id in map_ids {
        table.map_id().delete(map_id);
        table.insert(WorldImport {
            map_id,
            completed_at: ctx.timestamp,
        });
    }
    Ok(())
}

/// Refuse when any of `map_ids` lacks a completed World Import or its required content.
#[reducer]
pub fn check_world_content(ctx: &ReducerContext, map_ids: Vec<u32>) -> Result<(), String> {
    require_operator(ctx)?;
    let imports = ctx.db.game_world_import();
    content_problem(
        &map_ids,
        |map| imports.map_id().find(map).is_some(),
        |map| crate::vmap::active_generation_id(ctx, map).is_some(),
    )
}

/// The pure half of the content check, so the decision is testable without a database.
fn content_problem(
    map_ids: &[u32],
    completed: impl Fn(u32) -> bool,
    wmo_active: impl Fn(u32) -> bool,
) -> Result<(), String> {
    if map_ids.is_empty() {
        return Err("no maps named for the content check".to_string());
    }
    if let Some(map) = map_ids.iter().copied().find(|&map| !completed(map)) {
        return Err(format!("no completed world import for map {map}"));
    }
    if let Some(map) = map_ids
        .iter()
        .copied()
        .find(|map| WMO_MAPS.contains(map) && !wmo_active(*map))
    {
        return Err(format!("missing imported WMO geometry for map {map}"));
    }
    Ok(())
}

#[cfg(test)]
mod world_import_tests {
    use super::content_problem;

    #[test]
    fn complete_content_passes() {
        assert_eq!(
            content_problem(&[0, 1, 36, 369], |_| true, |_| true),
            Ok(())
        );
    }

    #[test]
    fn each_missing_part_names_itself() {
        let refused = |result: Result<(), String>| result.unwrap_err();
        assert!(refused(content_problem(&[], |_| true, |_| true)).contains("no maps"));
        assert_eq!(
            refused(content_problem(&[0, 1], |map| map == 0, |_| true)),
            "no completed world import for map 1"
        );
        assert_eq!(
            refused(content_problem(&[0, 36], |_| true, |_| false)),
            "missing imported WMO geometry for map 36"
        );
    }

    #[test]
    fn maps_without_an_importer_vmap_slice_need_no_wmo_geometry() {
        assert_eq!(content_problem(&[0, 1, 369], |_| true, |_| false), Ok(()));
    }
}
