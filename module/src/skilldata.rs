use spacetimedb::table;

#[table(accessor = game_skill_line, public)]
pub struct SkillLine {
    #[primary_key]
    pub skill_line: u32,
    pub category: u32,
    pub name: String,
}

#[table(accessor = game_skill_ability, public, index(accessor = by_skill_line, btree(columns = [skill_line])))]
pub struct SkillAbility {
    #[primary_key]
    #[auto_inc]
    pub id: u64,
    pub spell_id: u32,
    pub skill_line: u32,
    pub race_mask: u32,
    pub class_mask: u32,
    pub min_skill: i32,
    pub acquire_method: i32,
    pub gray: i32,
    pub green: i32,
}

#[table(accessor = game_skill_availability, public, index(accessor = by_skill_line, btree(columns = [skill_line])))]
pub struct SkillAvailability {
    #[primary_key]
    #[auto_inc]
    pub id: u64,
    pub skill_line: u32,
    pub race_mask: u32,
    pub class_mask: u32,
    pub flags: i32,
    pub min_level: i32,
}
