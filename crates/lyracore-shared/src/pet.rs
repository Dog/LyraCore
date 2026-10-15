// The client classifies creature GUIDs by their high 16 bits.
const HIGHGUID_UNIT: u64 = 0xF130;
const HIGHGUID_PET: u64 = 0xF140;

// Creature-owned guardians use HIGHGUID_PET to avoid colliding with their owner guid.
pub fn pet_guid_for(owner_guid: u64) -> u64 {
    let high = if owner_guid >> 48 == HIGHGUID_UNIT {
        HIGHGUID_PET
    } else {
        HIGHGUID_UNIT
    };
    (high << 48) | (owner_guid & 0x0000_FFFF_FFFF_FFFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_player_owners_pet_keeps_the_unit_namespace() {
        let owner = 7;
        assert_eq!(pet_guid_for(owner) >> 48, HIGHGUID_UNIT);
        assert_eq!(pet_guid_for(owner) & 0x0000_FFFF_FFFF_FFFF, 7);
    }

    #[test]
    fn a_creature_owners_guardian_uses_the_pet_namespace() {
        let owner = (HIGHGUID_UNIT << 48) | 11;
        assert_eq!(pet_guid_for(owner) >> 48, HIGHGUID_PET);
    }
}
