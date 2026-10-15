use spacetimedb::{table, ReducerContext, Table};

pub const MAX_COMBO: u32 = 5;

#[table(accessor = game_combo_point, public, index(accessor = by_owner, btree(columns = [owner_guid])))]
pub struct ComboPoint {
    #[primary_key]
    #[auto_inc]
    pub id: u64,
    pub owner_guid: u64,
    pub target_guid: u64,
    pub points: u32,
}

pub(crate) fn capped_inc(points: u32) -> u32 {
    (points + 1).min(MAX_COMBO)
}

pub(crate) fn finisher_damage(per_point: i32, combo: u32) -> i32 {
    per_point.saturating_mul(combo as i32)
}

pub(crate) fn finisher_duration_ms(base_ms: u32, combo: u32) -> u32 {
    base_ms + combo.min(MAX_COMBO) * 3_000
}

fn find_row(ctx: &ReducerContext, owner_guid: u64, target_guid: u64) -> Option<ComboPoint> {
    ctx.db
        .game_combo_point()
        .by_owner()
        .filter(&owner_guid)
        .find(|r| r.target_guid == target_guid)
}

pub(crate) fn combo_points(ctx: &ReducerContext, owner_guid: u64, target_guid: u64) -> u32 {
    find_row(ctx, owner_guid, target_guid)
        .map(|r| r.points)
        .unwrap_or(0)
}

pub(crate) fn add_combo_point(ctx: &ReducerContext, owner_guid: u64, target_guid: u64) {
    let combos = ctx.db.game_combo_point();
    if let Some(mut r) = find_row(ctx, owner_guid, target_guid) {
        r.points = capped_inc(r.points);
        combos.id().update(r);
    } else {
        combos.insert(ComboPoint {
            id: 0,
            owner_guid,
            target_guid,
            points: 1,
        });
    }
}

pub(crate) fn spend_combo(ctx: &ReducerContext, owner_guid: u64, target_guid: u64) {
    let combos = ctx.db.game_combo_point();
    if let Some(r) = find_row(ctx, owner_guid, target_guid) {
        combos.id().delete(r.id);
    }
}

#[cfg(test)]
mod tests {
    use super::capped_inc;

    #[test]
    fn combo_caps_at_five() {
        let mut p = 0;
        for _ in 0..8 {
            p = capped_inc(p);
        }
        assert_eq!(p, super::MAX_COMBO);
    }

    #[test]
    fn finisher_scales_with_points() {
        assert_eq!(super::finisher_damage(5, 3), 15);
        assert_eq!(super::finisher_damage(5, 0), 0);
    }

    #[test]
    fn finisher_duration_follows_the_vanilla_snd_curve() {
        assert_eq!(super::finisher_duration_ms(6000, 1), 9_000);
        assert_eq!(super::finisher_duration_ms(6000, 2), 12_000);
        assert_eq!(super::finisher_duration_ms(6000, 3), 15_000);
        assert_eq!(super::finisher_duration_ms(6000, 4), 18_000);
        assert_eq!(super::finisher_duration_ms(6000, 5), 21_000);
        assert_eq!(super::finisher_duration_ms(6000, 0), 6_000);
        assert_eq!(super::finisher_duration_ms(6000, 9), 21_000);
    }
}
