use crate::constants::gossip_condition as encoded;

/// Source kinds are signed: -3 NOT, -2 OR, -1 AND; operands can be condition ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Condition {
    pub kind: i32,
    pub values: [u32; 4],
    pub flags: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuestState {
    /// The quest template does not exist.
    Missing,
    Absent,
    Incomplete,
    Complete,
    Failed,
    Rewarded,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CharacterFacts {
    pub level: u32,
    pub race: u8,
    pub class: u8,
}

/// An unknown result cannot become eligible through NOT, reverse-result flags, or OR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    Missing(u32),
    MissingQuest(u32),
    Cycle(u32),
    Unsupported(i32),
    Operands(i32),
    Flags(u8),
    Limit,
}

/// Only `Ok(true)` permits showing or executing an option.
pub fn evaluate_option(
    kind: u32,
    value1: u32,
    value2: u32,
    character: CharacterFacts,
    quest: impl Fn(u32) -> QuestState,
    condition: impl Fn(u32) -> Option<Condition>,
) -> Result<bool, Unavailable> {
    let source_kind = match kind {
        encoded::NONE => return Ok(true),
        encoded::QUEST_TAKEN => 9,
        encoded::QUEST_REWARDED => 8,
        encoded::NEVER => return Ok(false),
        encoded::LEVEL => 15,
        encoded::TEAM => 6,
        encoded::RACE_CLASS => 14,
        encoded::QUEST_NONE => 22,
        encoded::REFERENCE => {
            return evaluate_reference(value1, character, quest, condition);
        }
        _ => return Err(Unavailable::Unsupported(kind as i32)),
    };
    Evaluation::new(character, &quest, &condition).row(Condition {
        kind: source_kind,
        values: [value1, value2, 0, 0],
        flags: 0,
    })
}

/// Missing, cyclic, or excessive source references return `Err`.
pub fn evaluate_reference(
    id: u32,
    character: CharacterFacts,
    quest: impl Fn(u32) -> QuestState,
    condition: impl Fn(u32) -> Option<Condition>,
) -> Result<bool, Unavailable> {
    Evaluation::new(character, &quest, &condition).reference(id)
}

struct Evaluation<'a, Q, C> {
    character: CharacterFacts,
    quest: &'a Q,
    condition: &'a C,
    path: Vec<u32>,
    remaining: usize,
}

impl<'a, Q, C> Evaluation<'a, Q, C>
where
    Q: Fn(u32) -> QuestState,
    C: Fn(u32) -> Option<Condition>,
{
    fn new(character: CharacterFacts, quest: &'a Q, condition: &'a C) -> Self {
        Self {
            character,
            quest,
            condition,
            path: Vec::new(),
            remaining: 256,
        }
    }

    fn reference(&mut self, id: u32) -> Result<bool, Unavailable> {
        if self.path.contains(&id) {
            return Err(Unavailable::Cycle(id));
        }
        if self.path.len() >= 32 || self.remaining == 0 {
            return Err(Unavailable::Limit);
        }
        self.remaining -= 1;
        let row = (self.condition)(id).ok_or(Unavailable::Missing(id))?;
        self.path.push(id);
        let result = self.row(row);
        self.path.pop();
        result
    }

    fn row(&mut self, row: Condition) -> Result<bool, Unavailable> {
        if row.flags & !1 != 0 {
            return Err(Unavailable::Flags(row.flags));
        }
        let [a, b, c, d] = row.values;
        if row.kind >= 0 && (c != 0 || d != 0) {
            return Err(Unavailable::Operands(row.kind));
        }
        let result = match row.kind {
            -3 if b == 0 && c == 0 && d == 0 => !self.reference(a)?,
            -2 | -1 => {
                let mut result = row.kind == -1;
                for id in [a, b]
                    .into_iter()
                    .chain([c, d].into_iter().filter(|id| *id != 0))
                {
                    // Evaluate every operand: a true branch cannot conceal an unsupported sibling.
                    let child = self.reference(id)?;
                    result = if row.kind == -1 {
                        result && child
                    } else {
                        result || child
                    };
                }
                result
            }
            0 if a == 0 && b == 0 => true,
            6 if b == 0 && matches!(a, 67 | 469) => {
                if !matches!(self.character.race, 1..=8) {
                    return Err(Unavailable::Operands(row.kind));
                }
                crate::faction::team_for_race(self.character.race) == a
            }
            8 if a != 0 && b == 0 => self.quest_state(a)? == QuestState::Rewarded,
            9 if a != 0 && b <= 2 => matches!(
                (self.quest_state(a)?, b),
                (QuestState::Incomplete, 0 | 1) | (QuestState::Complete, 0 | 2)
            ),
            14 => {
                // CMaNGOS loading requires a playable bit per nonzero mask and at least one mask.
                if (a == 0 && b == 0) || (a != 0 && a & 0xff == 0) || (b != 0 && b & 0x5df == 0) {
                    return Err(Unavailable::Operands(row.kind));
                }
                let race = self
                    .character
                    .race
                    .checked_sub(1)
                    .and_then(|v| 1u32.checked_shl(v.into()));
                let class = self
                    .character
                    .class
                    .checked_sub(1)
                    .and_then(|v| 1u32.checked_shl(v.into()));
                let (Some(race), Some(class)) = (race, class) else {
                    return Err(Unavailable::Operands(row.kind));
                };
                (a == 0 || race & a != 0) && (b == 0 || class & b != 0)
            }
            15 if a > 0 && a <= 60 && b <= 2 => match b {
                0 => self.character.level == a,
                1 => self.character.level >= a,
                _ => self.character.level <= a,
            },
            22 if a != 0 && b == 0 => {
                matches!(
                    self.quest_state(a)?,
                    QuestState::Absent | QuestState::Failed
                )
            }
            -3 | 0 | 6 | 8 | 9 | 15 | 22 => return Err(Unavailable::Operands(row.kind)),
            kind => return Err(Unavailable::Unsupported(kind)),
        };
        Ok(if row.flags & 1 != 0 { !result } else { result })
    }

    fn quest_state(&self, id: u32) -> Result<QuestState, Unavailable> {
        match (self.quest)(id) {
            QuestState::Missing => Err(Unavailable::MissingQuest(id)),
            state => Ok(state),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const HUMAN_WARRIOR: CharacterFacts = CharacterFacts {
        level: 20,
        race: 1,
        class: 1,
    };

    fn row(kind: i32, a: u32, b: u32) -> Condition {
        Condition {
            kind,
            values: [a, b, 0, 0],
            flags: 0,
        }
    }

    #[test]
    fn quest_taken_distinguishes_incomplete_complete_failed_and_rewarded() {
        for (state, expected) in [
            (QuestState::Absent, [false, false, false]),
            (QuestState::Incomplete, [true, true, false]),
            (QuestState::Complete, [true, false, true]),
            (QuestState::Failed, [false, false, false]),
            (QuestState::Rewarded, [false, false, false]),
        ] {
            for (operand, expected) in expected.into_iter().enumerate() {
                assert_eq!(
                    evaluate_option(
                        encoded::QUEST_TAKEN,
                        777,
                        operand as u32,
                        HUMAN_WARRIOR,
                        |_| state,
                        |_| None
                    ),
                    Ok(expected)
                );
            }
            assert_eq!(
                evaluate_option(
                    encoded::QUEST_REWARDED,
                    777,
                    0,
                    HUMAN_WARRIOR,
                    |_| state,
                    |_| None
                ),
                Ok(state == QuestState::Rewarded)
            );
        }
    }

    #[test]
    fn level_team_and_race_class_read_both_operands() {
        for (kind, a, b, expected) in [
            (encoded::LEVEL, 20, 0, true),
            (encoded::LEVEL, 21, 0, false),
            (encoded::LEVEL, 19, 1, true),
            (encoded::LEVEL, 21, 1, false),
            (encoded::LEVEL, 21, 2, true),
            (encoded::LEVEL, 19, 2, false),
            (encoded::TEAM, 469, 0, true),
            (encoded::TEAM, 67, 0, false),
            (encoded::RACE_CLASS, 1, 1, true),
            (encoded::RACE_CLASS, 2, 1, false),
            (encoded::RACE_CLASS, 1, 2, false),
            (encoded::RACE_CLASS, 0, 1, true),
            (encoded::RACE_CLASS, 1, 0, true),
            (encoded::RACE_CLASS, 0x101, 0x21, true),
        ] {
            assert_eq!(
                evaluate_option(kind, a, b, HUMAN_WARRIOR, |_| QuestState::Absent, |_| None),
                Ok(expected)
            );
        }
    }

    #[test]
    fn compound_references_and_reverse_flags_preserve_boolean_semantics() {
        let mut rows = HashMap::from([
            (1, row(15, 20, 0)),
            (2, row(8, 777, 0)),
            (3, row(-3, 2, 0)),
            (4, row(-1, 1, 3)),
            (5, row(-2, 1, 2)),
        ]);
        let eval = |id, rows: &HashMap<u32, Condition>| {
            evaluate_reference(
                id,
                HUMAN_WARRIOR,
                |_| QuestState::Absent,
                |id| rows.get(&id).copied(),
            )
        };
        assert_eq!(eval(2, &rows), Ok(false));
        assert_eq!(eval(3, &rows), Ok(true));
        assert_eq!(eval(4, &rows), Ok(true));
        assert_eq!(eval(5, &rows), Ok(true));
        rows.get_mut(&4).unwrap().flags = 1;
        assert_eq!(eval(4, &rows), Ok(false));
    }

    #[test]
    fn unsupported_missing_cyclic_and_invalid_conditions_stay_unavailable_under_negation() {
        for bad in [row(99, 0, 0), row(-3, 99, 0), row(-3, 2, 0), row(9, 777, 3)] {
            let rows = HashMap::from([
                (1, row(-3, 2, 0)),
                (2, bad),
                (3, row(0, 0, 0)),
                (4, row(-2, 3, 1)),
            ]);
            for id in [1, 4] {
                assert!(evaluate_reference(
                    id,
                    HUMAN_WARRIOR,
                    |_| QuestState::Absent,
                    |id| rows.get(&id).copied()
                )
                .is_err());
            }
            let mut reverse = bad;
            reverse.flags = 1;
            assert!(evaluate_reference(
                2,
                HUMAN_WARRIOR,
                |_| QuestState::Absent,
                |id| if id == 2 { Some(reverse) } else { None }
            )
            .is_err());
        }
    }

    #[test]
    fn work_is_bounded_even_when_an_acyclic_graph_reuses_subtrees() {
        let result = evaluate_reference(
            1,
            HUMAN_WARRIOR,
            |_| QuestState::Absent,
            |id| {
                Some(if id < 20 {
                    row(-1, id + 1, id + 1)
                } else {
                    row(0, 0, 0)
                })
            },
        );
        assert_eq!(result, Err(Unavailable::Limit));
    }

    #[test]
    fn invalid_race_class_masks_stay_unavailable_under_reverse_and_not() {
        for (race, class) in [(0, 0), (0x100, 0), (0, 0x20)] {
            for flags in [0, 1] {
                let mut invalid = row(14, race, class);
                invalid.flags = flags;
                let rows = HashMap::from([(1, invalid), (2, row(-3, 1, 0))]);
                for id in [1, 2] {
                    assert_eq!(
                        evaluate_reference(
                            id,
                            HUMAN_WARRIOR,
                            |_| QuestState::Absent,
                            |id| rows.get(&id).copied(),
                        ),
                        Err(Unavailable::Operands(14))
                    );
                }
            }
        }
    }

    #[test]
    fn a_missing_quest_subject_stays_unavailable_under_reverse_not_and_or() {
        for kind in [8, 9, 22] {
            for flags in [0, 1] {
                let mut missing_quest = row(kind, 777, 0);
                missing_quest.flags = flags;
                let rows = HashMap::from([
                    (1, missing_quest),
                    (2, row(-3, 1, 0)),
                    (3, row(0, 0, 0)),
                    (4, row(-2, 3, 1)),
                ]);
                for id in [1, 2, 4] {
                    assert_eq!(
                        evaluate_reference(
                            id,
                            HUMAN_WARRIOR,
                            |_| QuestState::Missing,
                            |id| rows.get(&id).copied(),
                        ),
                        Err(Unavailable::MissingQuest(777))
                    );
                }
            }
        }
    }
}
