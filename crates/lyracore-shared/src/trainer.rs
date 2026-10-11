//! Which characters a trainer will serve.
//!
//! Shared because the rule has two producers that must not drift: the module enforces it on the buy
//! and the respec, and the gateway applies the same rule to decide whether to show the window and
//! the gossip option at all. A display copy that drifted permissive would advertise a service the
//! module then refuses; one that drifted restrictive would hide a trainer that works.

/// `creature_template.TrainerType`.
///
/// `CLASS` is 0, which is also the ingested column's default, so most creatures read `CLASS` without
/// meaning anything by it. This value alone can never identify a class trainer — see [`serves`].
pub mod trainer_type {
    /// Teaches a class's spells — the only type this gate restricts.
    pub const CLASS: u8 = 0;
    /// Riding trainer. Serves every class.
    pub const MOUNTS: u8 = 1;
    /// Profession trainer and the weapon masters. Serves every class.
    pub const TRADESKILLS: u8 = 2;
    /// Hunter pet trainer. Serves every class here: vanilla also requires a Hunter, but some pet
    /// trainers carry a class id and there is no pet system to gate, so restricting would be worse.
    pub const PETS: u8 = 3;
}

/// `SkillLine.dbc` id of Riding — the skill a [`trainer_type::MOUNTS`] trainer teaches, and the line a
/// mount spell's `SkillLineAbility` row names. Shared for the same no-drift reason as [`serves`]: the
/// module grants the rank on the buy, and the gateway needs to know a riding offering teaches a SKILL so
/// it does not echo the offering's marker id back as a learned spell.
pub const RIDING_SKILL_LINE: u32 = 762;

/// Why the Module refused a trainer Durable Request. The tag is the whole reducer error text, so
/// neither tier matches on human prose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrainerRefusal {
    /// The trainer will not serve this interaction at all: missing, not a trainer, on another map,
    /// out of range, refusing the buyer's class, or asked for riding while teaching something else.
    Unavailable,
    /// The trainer's offering list does not carry that spell.
    NotOffered,
    AlreadyKnown,
    LevelTooLow,
    /// A rank purchase whose `game_spell_chain` predecessor is not known yet.
    PreviousRankMissing,
    NotEnoughMoney,
}

impl TrainerRefusal {
    pub const ALL: [Self; 6] = [
        Self::Unavailable,
        Self::NotOffered,
        Self::AlreadyKnown,
        Self::LevelTooLow,
        Self::PreviousRankMissing,
        Self::NotEnoughMoney,
    ];

    pub fn as_tag(self) -> &'static str {
        match self {
            Self::Unavailable => "trainer:unavailable",
            Self::NotOffered => "trainer:not_offered",
            Self::AlreadyKnown => "trainer:already_known",
            Self::LevelTooLow => "trainer:level_too_low",
            Self::PreviousRankMissing => "trainer:previous_rank_missing",
            Self::NotEnoughMoney => "trainer:not_enough_money",
        }
    }

    pub fn parse_tag(tag: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|refusal| refusal.as_tag() == tag)
    }
}

/// Will this trainer serve a character of `player_class`?
///
/// Refuses only when all three hold: the trainer is a `CLASS` trainer, its `trainer_class` is
/// populated, and it differs from the character's. The populated check is what keeps the gate
/// fail-open — `CLASS` is 0 and so is the column's default, so without it every trainer in a world
/// that has not been re-imported would refuse everyone.
///
/// Both ids are class IDs (1 Warrior, 2 Paladin, 3 Hunter, 4 Rogue, 5 Priest, 7 Shaman, 8 Mage,
/// 9 Warlock, 11 Druid), never masks.
pub const fn serves(player_class: u8, trainer_type: u8, trainer_class: u8) -> bool {
    trainer_type != self::trainer_type::CLASS || trainer_class == 0 || trainer_class == player_class
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every class the 1.12 client can create. 6 and 10 do not exist in vanilla.
    const CLASSES: [u8; 9] = [1, 2, 3, 4, 5, 7, 8, 9, 11];

    #[test]
    fn every_refusal_tag_round_trips() {
        for refusal in TrainerRefusal::ALL {
            assert_eq!(TrainerRefusal::parse_tag(refusal.as_tag()), Some(refusal));
        }
        assert_eq!(TrainerRefusal::parse_tag("trainer:"), None);
        assert_eq!(
            TrainerRefusal::parse_tag("gw_trainer_buy reducer timed out after 10s"),
            None
        );
    }

    #[test]
    fn a_class_trainer_serves_its_own_class_and_refuses_every_other() {
        for &trainer_class in &CLASSES {
            for &player_class in &CLASSES {
                assert_eq!(
                    serves(player_class, trainer_type::CLASS, trainer_class),
                    player_class == trainer_class,
                    "class trainer {trainer_class} vs player {player_class}"
                );
            }
        }
        // The reported case: a Warrior at a Paladin trainer.
        assert!(!serves(1, trainer_type::CLASS, 2));
        assert!(serves(2, trainer_type::CLASS, 2));
    }

    /// Fail-open: if this breaks, an un-reimported world locks every player out of every trainer.
    #[test]
    fn an_unpopulated_trainer_class_serves_every_class() {
        for &player_class in &CLASSES {
            assert!(
                serves(player_class, trainer_type::CLASS, 0),
                "un-reimported trainer must still serve class {player_class}"
            );
        }
    }

    /// A populated class id on a non-CLASS trainer is real data (pet trainers ship one), not a
    /// reason to gate.
    #[test]
    fn non_class_trainer_types_serve_every_class_even_with_a_populated_trainer_class() {
        for ty in [
            trainer_type::MOUNTS,
            trainer_type::TRADESKILLS,
            trainer_type::PETS,
        ] {
            for &player_class in &CLASSES {
                assert!(
                    serves(player_class, ty, 0),
                    "type {ty} serves {player_class}"
                );
                for &trainer_class in &CLASSES {
                    assert!(
                        serves(player_class, ty, trainer_class),
                        "type {ty} (class {trainer_class}) still serves {player_class}"
                    );
                }
            }
        }
    }

    /// The gate must read the columns the way the importer writes them.
    #[test]
    fn the_dump_verified_trainers_gate_as_ingested() {
        // Brother Sammuel (925), Paladin trainer: type 0 / class 2.
        assert!(serves(2, 0, 2));
        assert!(!serves(1, 0, 2));
        // Woo Ping (11867), weapon master: type 2 / class 0.
        for &c in &CLASSES {
            assert!(serves(c, 2, 0), "every class trains weapons at Woo Ping");
        }
        // Larimaine Purdue (2485), portal trainer: a genuine class trainer, so mages only.
        assert!(serves(8, 0, 8));
        assert!(!serves(1, 0, 8));
    }

    /// Drift guard: the ingest stores the dump's raw value, so these names must match the source enum.
    #[test]
    fn trainer_type_constants_match_the_source_enum() {
        assert_eq!(trainer_type::CLASS, 0);
        assert_eq!(trainer_type::MOUNTS, 1);
        assert_eq!(trainer_type::TRADESKILLS, 2);
        assert_eq!(trainer_type::PETS, 3);
    }
}

#[derive(Clone, Copy)]
pub struct LearnEffect {
    pub index: u8,
    pub id: u64,
    pub kind: u8,
    pub trigger_spell: u32,
}

pub fn learn_target_trigger(kind: u8, trigger_spell: u32) -> Option<u32> {
    use crate::spell::taxonomy::{
        A_FLAG, A_PERIODIC_TRIGGER, A_PROC_DAMAGE, A_PROC_TRIGGER, E_TRIGGER,
    };
    (trigger_spell != 0
        && ![
            A_PERIODIC_TRIGGER,
            A_FLAG,
            A_PROC_TRIGGER,
            A_PROC_DAMAGE,
            E_TRIGGER,
        ]
        .contains(&kind))
    .then_some(trigger_spell)
}

/// Resolve a wrapper independently of table iteration order.
pub fn resolve_learn_target(spell_id: u32, effects: impl IntoIterator<Item = LearnEffect>) -> u32 {
    effects
        .into_iter()
        .filter_map(|effect| {
            learn_target_trigger(effect.kind, effect.trigger_spell)
                .map(|target| (effect.index, effect.id, target))
        })
        .min_by_key(|&(index, id, _)| (index, id))
        .map_or(spell_id, |(_, _, target)| target)
}

pub fn is_combat_skill_line(line: u32) -> bool {
    matches!(
        line,
        44 | 172 | 54 | 160 | 229 | 43 | 55 | 136 | 473 | 173 | 162 | 95
    )
}

/// Combat skills are known by row presence; profession and riding tiers compare caps.
pub fn offering_known(
    skill_line: u32,
    offered_cap: u32,
    stored_cap: Option<u32>,
    knows_spell: bool,
) -> bool {
    if skill_line == 0 {
        knows_spell
    } else if is_combat_skill_line(skill_line) {
        stored_cap.is_some()
    } else {
        stored_cap.is_some_and(|cap| cap >= offered_cap)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OfferingState {
    Known,
    LevelTooLow,
    Available,
}

pub fn offering_state(known: bool, level: u32, required_level: u32) -> OfferingState {
    if known {
        OfferingState::Known
    } else if level < required_level {
        OfferingState::LevelTooLow
    } else {
        OfferingState::Available
    }
}

#[cfg(test)]
mod rule_tests {
    use super::*;

    #[test]
    fn wrapper_resolution_orders_eligible_effects_by_index_then_id() {
        let effects = [
            LearnEffect {
                index: 2,
                id: 1,
                kind: 0,
                trigger_spell: 100,
            },
            LearnEffect {
                index: 1,
                id: 8,
                kind: 0,
                trigger_spell: 200,
            },
            LearnEffect {
                index: 1,
                id: 7,
                kind: 0,
                trigger_spell: 300,
            },
        ];
        assert_eq!(resolve_learn_target(1873, effects), 300);
        assert_eq!(resolve_learn_target(1873, effects.into_iter().rev()), 300);
    }

    #[test]
    fn wrapper_resolution_ignores_all_trigger_and_proc_kinds() {
        let excluded = [0x93, 0xBE, 0xAB, 0xB4, 0x05];
        for kind in excluded {
            let effect = LearnEffect {
                index: 0,
                id: 1,
                kind,
                trigger_spell: 639,
            };
            assert_eq!(resolve_learn_target(1873, [effect]), 1873, "kind {kind:#x}");
        }
        assert_eq!(
            resolve_learn_target(
                1873,
                [LearnEffect {
                    index: 0,
                    id: 1,
                    kind: 0,
                    trigger_spell: 0
                }]
            ),
            1873
        );
        assert_eq!(
            resolve_learn_target(
                1873,
                [LearnEffect {
                    index: 0,
                    id: 1,
                    kind: 0,
                    trigger_spell: 639
                }]
            ),
            639
        );
    }

    #[test]
    fn weapon_proficiency_uses_row_presence_while_riding_and_professions_use_tiers() {
        assert!(offering_known(173, 300, Some(1), false));
        assert!(!offering_known(173, 0, None, true));
        for line in [185, 762] {
            assert!(offering_known(line, 75, Some(75), false));
            assert!(!offering_known(line, 150, Some(75), false));
            assert!(!offering_known(line, 75, None, true));
        }
        assert!(offering_known(0, 0, None, true));
        assert!(!offering_known(0, 0, Some(300), false));
    }

    #[test]
    fn known_offerings_stay_gray_even_below_the_required_level() {
        assert_eq!(offering_state(true, 1, 10), OfferingState::Known);
        assert_eq!(offering_state(false, 9, 10), OfferingState::LevelTooLow);
        assert_eq!(offering_state(false, 10, 10), OfferingState::Available);
    }
}
