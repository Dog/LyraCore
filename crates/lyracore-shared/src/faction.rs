//! Shared race-to-team mapping for module and gateway faction checks.

/// cmangos team-faction ids, as `game_graveyard_zone.faction` carries them (0 = both factions serve
/// a zone; 469 = Alliance-only; 67 = Horde-only).
pub const TEAM_ALLIANCE: u32 = 469;
pub const TEAM_HORDE: u32 = 67;

/// The team a `game_character.race` byte belongs to.
///
/// Unknown races default to Alliance, matching the currently imported Alliance-only content.
pub fn team_for_race(race: u8) -> u32 {
    match race {
        2 | 5 | 6 | 8 => TEAM_HORDE, // Orc / Undead / Tauren / Troll
        _ => TEAM_ALLIANCE,
    }
}

/// Whether two race bytes belong to the same team.
pub fn same_team(race_a: u8, race_b: u8) -> bool {
    team_for_race(race_a) == team_for_race(race_b)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mapping itself, both directions plus the unrecognized-byte default.
    #[test]
    fn every_race_byte_maps_to_a_team_and_an_unknown_one_defaults_alliance() {
        for alliance in [1, 3, 4, 7, 11] {
            assert_eq!(team_for_race(alliance), TEAM_ALLIANCE, "race {alliance}");
        }
        for horde in [2, 5, 6, 8] {
            assert_eq!(team_for_race(horde), TEAM_HORDE, "race {horde}");
        }
        assert_eq!(team_for_race(200), TEAM_ALLIANCE);
    }

    /// The predicate the mail gate reads: same side yes, opposing side no.
    #[test]
    fn same_team_pairs_races_by_side() {
        assert!(same_team(1, 3), "Human and Dwarf are one team");
        assert!(same_team(2, 6), "Orc and Tauren are one team");
        assert!(!same_team(1, 2), "Human and Orc are not");
    }
}

pub fn reputation_rank(standing: i32) -> u8 {
    match standing {
        42000.. => 7,
        21000.. => 6,
        9000.. => 5,
        3000.. => 4,
        0.. => 3,
        -3000.. => 2,
        -6000.. => 1,
        _ => 0,
    }
}

/// Explicit relations take precedence over group masks. Faction zero has no explicit relation.
pub fn relation_holds(
    asserting: &[u32; 4],
    vetoing: &[u32; 4],
    mask: u32,
    other_faction: u32,
    other_group: u32,
) -> bool {
    if other_faction != 0 {
        if asserting.contains(&other_faction) {
            return true;
        }
        if vetoing.contains(&other_faction) {
            return false;
        }
    }
    mask & other_group != 0
}

/// A standing row for a faction with a reputation bar overrides template hostility.
pub fn refuses_interaction(reputation: Option<(i32, bool)>, template_hostile: bool) -> bool {
    reputation.map_or(template_hostile, |(standing, at_war)| {
        at_war || reputation_rank(standing) <= 2
    })
}

#[cfg(test)]
mod rule_tests {
    use super::*;

    #[test]
    fn reputation_ranks_change_at_each_vanilla_threshold() {
        for (standing, rank) in [
            (-6000, 1),
            (-3000, 2),
            (0, 3),
            (3000, 4),
            (9000, 5),
            (21000, 6),
            (42000, 7),
        ] {
            assert_eq!(reputation_rank(standing), rank);
            assert_eq!(reputation_rank(standing - 1), rank - 1);
        }
        assert_eq!(reputation_rank(i32::MIN), 0);
        assert_eq!(reputation_rank(i32::MAX), 7);
    }

    #[test]
    fn explicit_hostility_beats_friendship_and_friendship_beats_the_mask() {
        assert!(relation_holds(&[7, 0, 0, 0], &[7, 0, 0, 0], 0, 7, 1));
        assert!(!relation_holds(&[0; 4], &[7, 0, 0, 0], 1, 7, 1));
        assert!(relation_holds(&[0; 4], &[0; 4], 1, 0, 1));
        assert!(!relation_holds(&[0; 4], &[0; 4], 0, 0, 1));
    }

    #[test]
    fn reputation_and_at_war_override_template_hostility() {
        assert!(refuses_interaction(Some((42000, true)), false));
        assert!(refuses_interaction(Some((-1, false)), false));
        assert!(!refuses_interaction(Some((0, false)), true));
        assert!(refuses_interaction(None, true));
        assert!(!refuses_interaction(None, false));
    }
}
