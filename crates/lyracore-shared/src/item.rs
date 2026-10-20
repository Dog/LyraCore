pub const fn is_carried_slot(slot: u8) -> bool {
    slot <= 38 || (slot >= 120 && slot <= 191)
}

pub fn carried_quantity(entry: u32, items: impl IntoIterator<Item = (u8, u32, u32)>) -> u32 {
    items
        .into_iter()
        .filter(|&(slot, item_entry, _)| is_carried_slot(slot) && item_entry == entry)
        .map(|(_, _, count)| count)
        .sum()
}

// Launcher subclass pairing is not enforced by the Module.
pub fn loaded_ammo<T>(
    items: impl IntoIterator<Item = T>,
    fields: impl Fn(&T) -> (u8, u32, Option<u8>),
) -> Option<T> {
    items
        .into_iter()
        .filter_map(|item| {
            let (slot, count, class) = fields(&item);
            (is_carried_slot(slot) && count > 0 && class == Some(6)).then_some((slot, item))
        })
        .min_by_key(|(slot, _)| *slot)
        .map(|(_, item)| item)
}

pub const fn grants_equipped_stats(slot: u8, max_durability: u32, durability: u32) -> bool {
    slot <= 18 && !(max_durability > 0 && durability == 0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemRefusal {
    ItemNotFound,
    InventoryFull,
    WrongSlot,
    CannotEquip,
    TwoHandedWeaponEquipped,
    NoProficiency,
    RequiredSkill,
    RequiredReputation,
    PlayerDead,
    BankUnavailable,
    ItemNotUsable,
    NotRightNow,
    Indestructible,
    BagNotEmpty,
    Internal,
}

impl ItemRefusal {
    pub const ALL: [Self; 15] = [
        Self::ItemNotFound,
        Self::InventoryFull,
        Self::WrongSlot,
        Self::CannotEquip,
        Self::TwoHandedWeaponEquipped,
        Self::NoProficiency,
        Self::RequiredSkill,
        Self::RequiredReputation,
        Self::PlayerDead,
        Self::BankUnavailable,
        Self::ItemNotUsable,
        Self::NotRightNow,
        Self::Indestructible,
        Self::BagNotEmpty,
        Self::Internal,
    ];

    pub fn as_tag(self) -> &'static str {
        match self {
            Self::ItemNotFound => "item:item_not_found",
            Self::InventoryFull => "item:inventory_full",
            Self::WrongSlot => "item:wrong_slot",
            Self::CannotEquip => "item:cannot_equip",
            Self::TwoHandedWeaponEquipped => "item:two_handed_weapon_equipped",
            Self::NoProficiency => "item:no_proficiency",
            Self::RequiredSkill => "item:required_skill",
            Self::RequiredReputation => "item:required_reputation",
            Self::PlayerDead => "item:player_dead",
            Self::BankUnavailable => "item:bank_unavailable",
            Self::ItemNotUsable => "item:item_not_usable",
            Self::NotRightNow => "item:not_right_now",
            Self::Indestructible => "item:indestructible",
            Self::BagNotEmpty => "item:bag_not_empty",
            Self::Internal => "item:internal",
        }
    }

    pub fn parse_tag(tag: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|refusal| refusal.as_tag() == tag)
    }
}

pub mod armor_subclass {
    pub const MISC: u8 = 0;
    pub const CLOTH: u8 = 1;
    pub const LEATHER: u8 = 2;
    pub const MAIL: u8 = 3;
    pub const PLATE: u8 = 4;
    pub const SHIELD: u8 = 6;
}

pub mod item_class {
    pub const WEAPON: u8 = 2;
    pub const ARMOR: u8 = 4;
}

pub mod weapon_subclass {
    pub const AXE_1H: u8 = 0;
    pub const AXE_2H: u8 = 1;
    pub const BOW: u8 = 2;
    pub const GUN: u8 = 3;
    pub const MACE_1H: u8 = 4;
    pub const MACE_2H: u8 = 5;
    pub const POLEARM: u8 = 6;
    pub const SWORD_1H: u8 = 7;
    pub const SWORD_2H: u8 = 8;
    pub const STAFF: u8 = 10;
    pub const FIST: u8 = 13;
    pub const DAGGER: u8 = 15;
    pub const THROWN: u8 = 16;
    pub const CROSSBOW: u8 = 18;
    pub const WAND: u8 = 19;
    pub const FISHING_POLE: u8 = 20;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Proficiency {
    player_class: u8,
    armor_subclass_mask: u32,
}

impl Proficiency {
    pub const fn derive(player_class: u8, knows_plate: bool, knows_mail_upgrade: bool) -> Self {
        use armor_subclass::*;

        let cloth = bit(MISC) | bit(CLOTH);
        let mut mask = match player_class {
            1 | 2 => cloth | bit(LEATHER) | bit(MAIL) | bit(SHIELD),
            3 | 4 | 11 => cloth | bit(LEATHER),
            7 => cloth | bit(LEATHER) | bit(SHIELD),
            5 | 8 | 9 => cloth,
            _ => bit(MISC),
        };
        if knows_plate && matches!(player_class, 1 | 2) {
            mask |= bit(PLATE);
        }
        if knows_mail_upgrade && matches!(player_class, 3 | 7) {
            mask |= bit(MAIL);
        }
        Self {
            player_class,
            armor_subclass_mask: mask,
        }
    }

    pub fn from_spellbook(player_class: u8, knows: impl Fn(u32) -> bool) -> Self {
        use crate::constants::armor_proficiency::{MAIL_PASSIVE_SPELL_ID, PLATE_PASSIVE_SPELL_ID};

        Self::derive(
            player_class,
            knows(PLATE_PASSIVE_SPELL_ID),
            knows(MAIL_PASSIVE_SPELL_ID),
        )
    }

    // Auction filtering uses the class ceiling; equipping must use the learned passives.
    pub const fn class_ceiling(player_class: u8) -> Self {
        Self::derive(player_class, true, true)
    }

    // SMSG_SET_PROFICIENCY encodes armor subclasses as 1 << subclass.
    pub const fn armor_subclass_mask(self) -> u32 {
        self.armor_subclass_mask
    }

    pub const fn can_equip(self, item_class: u8, item_subclass: u8) -> bool {
        match item_class {
            item_class::ARMOR => self.allows_armor(item_subclass),
            item_class::WEAPON => weapon_proficiency(self.player_class, item_subclass),
            _ => true,
        }
    }

    pub const fn allows_armor(self, subclass: u8) -> bool {
        subclass < u32::BITS as u8 && self.armor_subclass_mask & bit(subclass) != 0
    }
}

const fn bit(subclass: u8) -> u32 {
    1 << subclass
}

const fn weapon_proficiency(player_class: u8, subclass: u8) -> bool {
    use weapon_subclass::*;

    if subclass == FISHING_POLE {
        return true;
    }
    match player_class {
        1 => subclass != WAND,
        2 => matches!(
            subclass,
            AXE_1H | AXE_2H | MACE_1H | MACE_2H | POLEARM | SWORD_1H | SWORD_2H
        ),
        3 => matches!(
            subclass,
            AXE_1H
                | AXE_2H
                | BOW
                | CROSSBOW
                | DAGGER
                | FIST
                | GUN
                | POLEARM
                | STAFF
                | SWORD_1H
                | SWORD_2H
                | THROWN
        ),
        4 => matches!(
            subclass,
            BOW | CROSSBOW | DAGGER | FIST | GUN | MACE_1H | SWORD_1H | THROWN
        ),
        5 => matches!(subclass, DAGGER | MACE_1H | STAFF | WAND),
        7 => matches!(
            subclass,
            AXE_1H | AXE_2H | DAGGER | FIST | MACE_1H | MACE_2H | STAFF
        ),
        8 | 9 => matches!(subclass, DAGGER | STAFF | SWORD_1H | WAND),
        11 => matches!(subclass, DAGGER | FIST | MACE_1H | MACE_2H | STAFF),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use armor_subclass::*;

    #[test]
    fn carried_quantity_excludes_bank_and_unknown_slots() {
        assert_eq!(
            carried_quantity(
                7,
                [
                    (0, 7, 1),
                    (38, 7, 2),
                    (39, 7, 40),
                    (119, 7, 40),
                    (120, 7, 3),
                    (191, 7, 4),
                    (192, 7, 40),
                    (23, 8, 50)
                ]
            ),
            10
        );
    }

    #[test]
    fn loaded_ammo_uses_the_lowest_carried_nonempty_projectile() {
        let items = [
            (39, 20, Some(6)),
            (120, 5, Some(6)),
            (191, 7, Some(6)),
            (23, 0, Some(6)),
            (24, 8, Some(2)),
            (25, 1, None),
        ];
        assert_eq!(loaded_ammo(items, |item| *item), Some((120, 5, Some(6))));
    }

    const PLAYABLE: [u8; 9] = [1, 2, 3, 4, 5, 7, 8, 9, 11];

    #[test]
    fn every_refusal_tag_round_trips() {
        for refusal in ItemRefusal::ALL {
            assert_eq!(ItemRefusal::parse_tag(refusal.as_tag()), Some(refusal));
        }
        assert_eq!(ItemRefusal::parse_tag("item:"), None);
        assert_eq!(
            ItemRefusal::parse_tag("gw_move_item reducer timed out after 10s"),
            None
        );
    }

    fn ceiling_can_equip(player_class: u8, item_class: u8, item_subclass: u8) -> bool {
        Proficiency::class_ceiling(player_class).can_equip(item_class, item_subclass)
    }

    fn armor_set(proficiency: Proficiency) -> Vec<u8> {
        (0..=SHIELD)
            .filter(|&subclass| proficiency.allows_armor(subclass))
            .collect()
    }

    #[test]
    fn untrained_classes_wear_their_vanilla_base_set() {
        let base = |class| armor_set(Proficiency::derive(class, false, false));
        assert_eq!(base(1), vec![MISC, CLOTH, LEATHER, MAIL, SHIELD]);
        assert_eq!(base(2), vec![MISC, CLOTH, LEATHER, MAIL, SHIELD]);
        assert_eq!(base(3), vec![MISC, CLOTH, LEATHER]);
        assert_eq!(base(4), vec![MISC, CLOTH, LEATHER]);
        assert_eq!(base(11), vec![MISC, CLOTH, LEATHER]);
        assert_eq!(base(7), vec![MISC, CLOTH, LEATHER, SHIELD]);
        for caster in [5u8, 8, 9] {
            assert_eq!(
                base(caster),
                vec![MISC, CLOTH],
                "class {caster} wears cloth"
            );
        }
    }

    #[test]
    fn the_plate_upgrade_only_moves_warriors_and_paladins() {
        for class in PLAYABLE {
            let untrained = Proficiency::derive(class, false, false);
            let trained = Proficiency::derive(class, true, false);
            if matches!(class, 1 | 2) {
                assert!(
                    !untrained.allows_armor(PLATE),
                    "class {class} starts unplated"
                );
                assert!(
                    trained.allows_armor(PLATE),
                    "class {class} trains plate at 40"
                );
            } else {
                assert_eq!(
                    armor_set(trained),
                    armor_set(untrained),
                    "class {class} cannot train plate, so the flag must change nothing"
                );
            }
        }
    }

    #[test]
    fn the_mail_upgrade_only_moves_hunters_and_shamans() {
        for class in PLAYABLE {
            let untrained = Proficiency::derive(class, false, false);
            let trained = Proficiency::derive(class, false, true);
            if matches!(class, 3 | 7) {
                assert!(
                    !untrained.allows_armor(MAIL),
                    "class {class} starts in leather"
                );
                assert!(
                    trained.allows_armor(MAIL),
                    "class {class} trains mail at 40"
                );
            } else {
                assert_eq!(
                    armor_set(trained),
                    armor_set(untrained),
                    "class {class} cannot train the mail upgrade"
                );
            }
        }
    }

    #[test]
    fn a_caster_holding_both_passives_still_wears_cloth() {
        let mage = Proficiency::derive(8, true, true);
        assert_eq!(armor_set(mage), vec![MISC, CLOTH]);
        assert!(!mage.can_equip(item_class::ARMOR, PLATE));
        assert!(!mage.can_equip(item_class::ARMOR, MAIL));
    }

    #[test]
    fn unknown_classes_and_subclasses_fail_closed() {
        for unknown in [0u8, 6, 10, 12, 255] {
            let proficiency = Proficiency::derive(unknown, true, true);
            assert_eq!(
                armor_set(proficiency),
                vec![MISC],
                "class {unknown} is not playable and must not wear armor"
            );
        }
        let warrior = Proficiency::derive(1, true, true);
        for subclass in [5u8, 7, 31, 32, 200, 255] {
            assert!(
                !warrior.allows_armor(subclass),
                "armor subclass {subclass} is not a tier and must be refused"
            );
        }
    }

    #[test]
    fn the_wire_mask_agrees_with_the_gate_for_every_class_and_flag() {
        for class in PLAYABLE {
            for knows_plate in [false, true] {
                for knows_mail in [false, true] {
                    let proficiency = Proficiency::derive(class, knows_plate, knows_mail);
                    let mask = proficiency.armor_subclass_mask();
                    for subclass in 0..=SHIELD {
                        assert_eq!(
                            mask & (1 << subclass) != 0,
                            proficiency.allows_armor(subclass),
                            "class {class} ({knows_plate}, {knows_mail}) disagrees on subclass {subclass}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn armor_training_never_changes_the_weapon_table() {
        for class in PLAYABLE {
            for subclass in 0..=weapon_subclass::FISHING_POLE {
                assert_eq!(
                    Proficiency::derive(class, false, false)
                        .can_equip(item_class::WEAPON, subclass),
                    Proficiency::derive(class, true, true).can_equip(item_class::WEAPON, subclass),
                    "class {class} weapon subclass {subclass}"
                );
            }
        }
    }

    #[test]
    fn the_class_ceiling_is_the_fully_trained_derivation() {
        for class in PLAYABLE {
            assert_eq!(
                Proficiency::class_ceiling(class),
                Proficiency::derive(class, true, true)
            );
        }
        assert!(ceiling_can_equip(1, item_class::ARMOR, PLATE));
        assert!(ceiling_can_equip(3, item_class::ARMOR, MAIL));
    }

    #[test]
    fn representative_class_proficiencies_are_available_to_gateway_reads() {
        assert!(!ceiling_can_equip(8, 4, 4));
        assert!(ceiling_can_equip(1, 4, 4));
        assert!(ceiling_can_equip(8, 2, 19));
        assert!(!ceiling_can_equip(1, 2, 19));
        assert!(ceiling_can_equip(8, 7, 0));
    }

    #[test]
    fn the_spellbook_reader_maps_the_two_passives_and_ignores_the_rest() {
        use crate::constants::armor_proficiency::{MAIL_PASSIVE_SPELL_ID, PLATE_PASSIVE_SPELL_ID};

        let from = |class, book: &[u32]| {
            let owned = book.to_vec();
            Proficiency::from_spellbook(class, move |spell| owned.contains(&spell))
        };
        for class in PLAYABLE {
            assert_eq!(from(class, &[]), Proficiency::derive(class, false, false));
            assert_eq!(
                from(class, &[PLATE_PASSIVE_SPELL_ID]),
                Proficiency::derive(class, true, false)
            );
            assert_eq!(
                from(class, &[MAIL_PASSIVE_SPELL_ID]),
                Proficiency::derive(class, false, true)
            );
            assert_eq!(
                from(class, &[PLATE_PASSIVE_SPELL_ID, MAIL_PASSIVE_SPELL_ID]),
                Proficiency::class_ceiling(class)
            );
            assert_eq!(
                from(class, &[7109, 8738, 674, 201]),
                Proficiency::derive(class, false, false),
                "class {class}: only the passives move armor"
            );
        }
    }
}
