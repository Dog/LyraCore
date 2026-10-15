use crate::spell::taxonomy::A_MOD_RESISTANCE;
use crate::{
    aura::{stacked_amount, AuraAmount},
    item_property as stat,
};

pub const SCHOOL_BITS: [u8; 7] = [1, 2, 4, 8, 16, 32, 64];
pub const STAT_KINDS: [u8; 7] = [
    stat::ARMOR,
    stat::HOLY_RESISTANCE,
    stat::FIRE_RESISTANCE,
    stat::NATURE_RESISTANCE,
    stat::FROST_RESISTANCE,
    stat::SHADOW_RESISTANCE,
    stat::ARCANE_RESISTANCE,
];

#[derive(Clone, Copy, Debug)]
pub struct ResistanceItem {
    pub slot: u8,
    pub max_durability: u32,
    pub durability: u32,
    pub template: [i32; 7],
    pub enchant_id: u32,
    pub property_enchants: [u32; 3],
}

fn school_index(school_mask: u8) -> Option<usize> {
    let index = school_mask.trailing_zeros() as usize;
    (index < SCHOOL_BITS.len()).then_some(index)
}

pub fn aura_contribution(aura: AuraAmount, school_mask: u8) -> i32 {
    if aura.kind == A_MOD_RESISTANCE && aura.parameter as u32 & u32::from(school_mask) != 0 {
        stacked_amount(aura.amount, aura.stacks)
    } else {
        0
    }
}

pub fn resistance_bonus(
    school_mask: u8,
    auras: impl IntoIterator<Item = AuraAmount>,
    items: impl IntoIterator<Item = ResistanceItem>,
    mut enchant_stat: impl FnMut(u32, u8) -> i32,
) -> i32 {
    let aura: i32 = auras
        .into_iter()
        .map(|a| aura_contribution(a, school_mask))
        .sum();
    let gear = school_index(school_mask).map_or(0, |index| {
        let kind = STAT_KINDS[index];
        stat::equipped_stat(
            items
                .into_iter()
                .filter(|item| {
                    crate::item::grants_equipped_stats(
                        item.slot,
                        item.max_durability,
                        item.durability,
                    )
                })
                .map(|item| {
                    let enchant = if item.enchant_id == 0 {
                        0
                    } else {
                        enchant_stat(item.enchant_id, kind)
                    };
                    let property = stat::property_stat(item.property_enchants, |id| {
                        if id == 0 {
                            0
                        } else {
                            enchant_stat(id, kind)
                        }
                    });
                    (
                        item.slot,
                        item.max_durability,
                        item.durability,
                        stat::item_stat(item.template[index], enchant, property),
                    )
                }),
        )
    });
    aura.saturating_add(gear)
}

pub fn effective_resistance(base: u32, bonus: i32) -> u32 {
    (i64::from(base) + i64::from(bonus)).clamp(0, i64::from(u32::MAX)) as u32
}

pub fn positive_armor(auras: impl IntoIterator<Item = AuraAmount>) -> u32 {
    auras
        .into_iter()
        .map(|a| aura_contribution(a, SCHOOL_BITS[0]))
        .filter(|&amount| amount > 0)
        .fold(0u32, |sum, amount| sum.saturating_add(amount as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chest() -> ResistanceItem {
        ResistanceItem {
            slot: 4,
            max_durability: 100,
            durability: 100,
            template: [120, 1, 2, 3, 4, 5, 6],
            enchant_id: 1,
            property_enchants: [2, 3, 0],
        }
    }

    #[test]
    fn resistance_combines_stack_masks_equipment_and_property_effects() {
        let auras = [
            AuraAmount {
                kind: A_MOD_RESISTANCE,
                parameter: 5,
                amount: 20,
                stacks: 0,
            },
            AuraAmount {
                kind: A_MOD_RESISTANCE,
                parameter: 1,
                amount: -10,
                stacks: 3,
            },
            AuraAmount {
                kind: 0,
                parameter: 1,
                amount: 500,
                stacks: 1,
            },
        ];
        let items = [
            chest(),
            ResistanceItem {
                slot: 23,
                ..chest()
            },
            ResistanceItem {
                durability: 0,
                ..chest()
            },
            ResistanceItem {
                slot: 18,
                max_durability: 0,
                durability: 0,
                enchant_id: 0,
                property_enchants: [0; 3],
                template: [5; 7],
            },
        ];
        let enchant = |id, kind| match (id, kind) {
            (1, stat::ARMOR) => 11,
            (2, stat::ARMOR) => 7,
            (3, stat::ARMOR) => -2,
            (2, stat::FIRE_RESISTANCE) => 9,
            _ => 0,
        };
        assert_eq!(
            effective_resistance(40, resistance_bonus(1, auras, items, enchant)),
            171
        );
        assert_eq!(resistance_bonus(4, auras, items, enchant), 36);
        assert_eq!(resistance_bonus(5, auras, items, enchant), 131);
        assert_eq!(positive_armor(auras), 20);
    }

    #[test]
    fn signed_bonus_saturates_before_base_and_final_clamp() {
        let item = ResistanceItem {
            template: [i32::MAX; 7],
            ..chest()
        };
        let aura = AuraAmount {
            kind: A_MOD_RESISTANCE,
            parameter: 1,
            amount: 10,
            stacks: 1,
        };
        assert_eq!(
            effective_resistance(40, resistance_bonus(1, [aura], [item], |_, _| 0)),
            i32::MAX as u32 + 40
        );
        assert_eq!(effective_resistance(40, -100), 0);
        assert_eq!(effective_resistance(u32::MAX, 10), u32::MAX);
        assert_eq!(resistance_bonus(0, [aura], [item], |_, _| 100), 0);
        assert_eq!(resistance_bonus(128, [aura], [item], |_, _| 100), 0);
    }
}
