use spacetimedb::table;

#[table(accessor = game_mail_template)]
pub struct MailTemplate {
    #[primary_key]
    pub id: u32,
    pub body: String,
}

#[table(accessor = game_mail_loot)]
pub struct MailLoot {
    #[primary_key]
    pub mail_template_id: u32,
    pub item_entry: u32,
    pub count: u32,
}

#[table(accessor = game_quest_reward_mail)]
pub struct QuestRewardMail {
    #[primary_key]
    pub quest_entry: u32,
    pub mail_template_id: u32,
    pub delay_secs: u32,
    pub sender_creature_entry: u32,
    pub money: u32,
}
