pub fn respec_cost_copper(respec_count: u32) -> u32 {
    match respec_count.min(10) {
        0 => 10_000,
        1 => 50_000,
        n => 100_000 + 50_000 * (n - 2),
    }
}

pub fn points_available(level: u32, spent: u32) -> u32 {
    level.saturating_sub(9).saturating_sub(spent)
}

pub fn rank_spell(rank: u8, spells: [u32; 5]) -> u32 {
    if rank == 0 {
        return 0;
    }
    let selected = spells[usize::from(rank.min(5) - 1)];
    if selected == 0 {
        spells[0]
    } else {
        selected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_points_remain_unsigned_and_never_underflow() {
        assert_eq!(points_available(9, 0), 0);
        assert_eq!(points_available(10, 0), 1);
        assert_eq!(points_available(60, 40), 11);
        assert_eq!(points_available(60, 52), 0);
        assert_eq!(points_available(u32::MAX, 0), 4_294_967_286);
    }

    #[test]
    fn rank_zero_is_unlearned_and_higher_ranks_use_rank_five_or_the_base() {
        assert_eq!(rank_spell(0, [100, 200, 300, 400, 500]), 0);
        assert_eq!(rank_spell(2, [100, 200, 300, 400, 500]), 200);
        assert_eq!(rank_spell(6, [100, 200, 300, 400, 500]), 500);
        assert_eq!(rank_spell(5, [100, 0, 0, 0, 0]), 100);
    }
}
