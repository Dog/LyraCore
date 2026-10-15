use core::fmt;

use crate::ids::{
    FIXTURE_CREATURE_ID_CEIL, FIXTURE_CREATURE_ID_FLOOR, FIXTURE_RESERVED_ID_CEIL,
    FIXTURE_RESERVED_ID_FLOOR, FIXTURE_SPELL_ID_CEIL, FIXTURE_SPELL_ID_FLOOR,
    MAX_QUEST_OBJECTIVE_INDEX, MAX_QUEST_REWARD_CHOICE_INDEX, MAX_SPELL_EFFECT_INDEX,
    MAX_STATS_LEVEL, PACKAGE_CAST_ID_CEIL, PACKAGE_CAST_ID_FLOOR, PACKAGE_CREATURE_AI_ID_CEIL,
    PACKAGE_CREATURE_AI_ID_FLOOR, PACKAGE_CREATURE_ID_CEIL, PACKAGE_CREATURE_ID_FLOOR,
    PACKAGE_GAMEOBJECT_ID_CEIL, PACKAGE_GAMEOBJECT_ID_FLOOR, PACKAGE_GLOBALS_ID_CEIL,
    PACKAGE_GLOBALS_ID_FLOOR, PACKAGE_GOSSIP_ID_CEIL, PACKAGE_GOSSIP_ID_FLOOR,
    PACKAGE_ITEM_ID_CEIL, PACKAGE_ITEM_ID_FLOOR, PACKAGE_LOOT_ID_CEIL, PACKAGE_LOOT_ID_FLOOR,
    PACKAGE_QUEST_ID_CEIL, PACKAGE_QUEST_ID_FLOOR, PACKAGE_SCRIPT_ID_CEIL, PACKAGE_SCRIPT_ID_FLOOR,
    PACKAGE_SPELLMETA_ID_CEIL, PACKAGE_SPELLMETA_ID_FLOOR, PACKAGE_SPELL_ID_CEIL,
    PACKAGE_SPELL_ID_FLOOR, PACKAGE_TRAINER_ID_CEIL, PACKAGE_TRAINER_ID_FLOOR,
};
use crate::schema::{FieldType, Table};
use crate::script::HOOK_EVENT_NAMES;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeltaError {
    Malformed(String),
    WrongArtifactKind {
        expected: &'static str,
        found: String,
    },
    UnsupportedVersion {
        found: String,
    },
    MissingMember {
        path: String,
    },
    UnexpectedMember {
        path: String,
        member: String,
    },
    WrongJsonType {
        path: String,
        expected: &'static str,
    },
    InvalidPackageId {
        found: String,
    },
    InvalidSourceHash {
        found: String,
    },
    UnknownTable {
        found: String,
    },
    UnknownOperation {
        found: String,
    },
    DeleteNotSupported,
    MalformedKey {
        table: Table,
        detail: String,
    },
    SpellIdNotClientSafe {
        spell_id: u32,
    },
    SpellIdFixtureReserved {
        spell_id: u32,
    },
    EffectIndexOutOfRange {
        effect_index: u8,
    },
    ItemIdNotClientSafe {
        entry: u32,
    },
    ItemIdFixtureReserved {
        entry: u32,
    },
    QuestIdNotClientSafe {
        entry: u32,
    },
    QuestIdFixtureReserved {
        entry: u32,
    },
    QuestObjectiveIndexOutOfRange {
        obj_index: u8,
    },
    QuestRewardChoiceIndexOutOfRange {
        choice_index: u8,
    },
    LootIdNotClientSafe {
        id: u64,
    },
    LootIdFixtureReserved {
        id: u64,
    },
    CastIdNotClientSafe {
        id: u64,
    },
    CastIdFixtureReserved {
        id: u64,
    },
    TrainerIdNotClientSafe {
        id: u64,
    },
    TrainerIdFixtureReserved {
        id: u64,
    },
    GossipIdNotClientSafe {
        id: u64,
    },
    GossipIdFixtureReserved {
        id: u64,
    },
    GlobalsIdNotClientSafe {
        id: u64,
    },
    GlobalsIdFixtureReserved {
        id: u64,
    },
    SpellmetaIdNotClientSafe {
        id: u64,
    },
    SpellmetaIdFixtureReserved {
        id: u64,
    },
    CreatureIdNotClientSafe {
        id: u32,
    },
    CreatureIdFixtureReserved {
        id: u32,
    },
    GameobjectIdNotClientSafe {
        id: u32,
    },
    GameobjectIdFixtureReserved {
        id: u32,
    },
    CreatureAiIdNotClientSafe {
        id: u64,
    },
    CreatureAiIdFixtureReserved {
        id: u64,
    },
    StatsLevelOutOfRange {
        level: u8,
    },
    InsertNotSupported {
        table: Table,
    },
    UnknownField {
        table: Table,
        field: String,
    },
    UnknownFieldType {
        field: String,
        found: String,
    },
    FieldTypeMismatch {
        table: Table,
        field: String,
        expected: FieldType,
        found: FieldType,
    },
    ValueOutOfRange {
        field: String,
        ty: FieldType,
        literal: String,
    },
    KeyColumnClaimed {
        table: Table,
        field: String,
    },
    IncompleteInsert {
        table: Table,
        missing: Vec<String>,
    },
    EmptyUpdate {
        table: Table,
    },
    DuplicateClaim {
        table: Table,
        key: String,
    },

    Script(ScriptRefusal),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptRefusal {
    IdNotInPackageBand { script_id: u32 },
    InvalidName { found: String },
    UnknownEvent { found: String, package: String },
    EmptySource { name: String },
    DuplicateId { script_id: u32 },
    DuplicateName { name: String },
}

impl fmt::Display for ScriptRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IdNotInPackageBand { script_id } => write!(
                f,
                "script {script_id} is outside the Package script range \
                 {PACKAGE_SCRIPT_ID_FLOOR}..={PACKAGE_SCRIPT_ID_CEIL}; a shipped Runtime Script \
                 must use an identifier the range clears for Packages"
            ),
            Self::InvalidName { found } => write!(
                f,
                "`{found}` is not a Runtime Script name: expected 1 to 64 characters of \
                 a-z, 0-9, `-`, `_` or `.`"
            ),
            Self::UnknownEvent { found, package } => write!(
                f,
                "unknown event `{found}`; a Runtime Script binds to one of {}, or to a Package \
                 Event of its own Package spelled `{package}.<name>`, where `<name>` is a \
                 lowercase letter followed by lowercase letters, digits or `_`",
                known_events()
            ),
            Self::EmptySource { name } => write!(
                f,
                "Runtime Script `{name}` carries no source; a script that should do nothing is \
                 shipped disabled, not empty"
            ),
            Self::DuplicateId { script_id } => write!(
                f,
                "script {script_id} is shipped twice by one Package; every Runtime Script needs \
                 its own identifier"
            ),
            Self::DuplicateName { name } => write!(
                f,
                "Runtime Script name `{name}` is shipped twice by one Package; every Runtime \
                 Script needs its own name"
            ),
        }
    }
}

impl fmt::Display for DeltaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SpellIdNotClientSafe { .. }
            | Self::SpellIdFixtureReserved { .. }
            | Self::EffectIndexOutOfRange { .. }
            | Self::ItemIdNotClientSafe { .. }
            | Self::ItemIdFixtureReserved { .. }
            | Self::QuestIdNotClientSafe { .. }
            | Self::QuestIdFixtureReserved { .. }
            | Self::QuestObjectiveIndexOutOfRange { .. }
            | Self::QuestRewardChoiceIndexOutOfRange { .. }
            | Self::LootIdNotClientSafe { .. }
            | Self::LootIdFixtureReserved { .. }
            | Self::CastIdNotClientSafe { .. }
            | Self::CastIdFixtureReserved { .. }
            | Self::TrainerIdNotClientSafe { .. }
            | Self::TrainerIdFixtureReserved { .. }
            | Self::GossipIdNotClientSafe { .. }
            | Self::GossipIdFixtureReserved { .. }
            | Self::GlobalsIdNotClientSafe { .. }
            | Self::GlobalsIdFixtureReserved { .. }
            | Self::SpellmetaIdNotClientSafe { .. }
            | Self::SpellmetaIdFixtureReserved { .. }
            | Self::CreatureIdNotClientSafe { .. }
            | Self::CreatureIdFixtureReserved { .. }
            | Self::GameobjectIdNotClientSafe { .. }
            | Self::GameobjectIdFixtureReserved { .. }
            | Self::CreatureAiIdNotClientSafe { .. }
            | Self::CreatureAiIdFixtureReserved { .. }
            | Self::StatsLevelOutOfRange { .. } => fmt_identifier_policy(self, f),
            Self::Script(refusal) => refusal.fmt(f),
            other => fmt_general(other, f),
        }
    }
}

fn fmt_identifier_policy(err: &DeltaError, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match err {
        DeltaError::SpellIdNotClientSafe { spell_id } => write!(
            f,
            "spell {spell_id} is outside the Package spell range \
             {PACKAGE_SPELL_ID_FLOOR}..={PACKAGE_SPELL_ID_CEIL}; an inserted spell must use an \
             identifier no client and no import can already own"
        ),
        DeltaError::ItemIdNotClientSafe { entry } => write!(
            f,
            "item {entry} is outside the Package item range \
             {PACKAGE_ITEM_ID_FLOOR}..={PACKAGE_ITEM_ID_CEIL}; an inserted item must use an \
             identifier no client and no import can already own"
        ),
        DeltaError::QuestIdNotClientSafe { entry } => write!(
            f,
            "quest {entry} is outside the Package quest range \
             {PACKAGE_QUEST_ID_FLOOR}..={PACKAGE_QUEST_ID_CEIL}; an inserted quest must use an \
             identifier no client and no import can already own"
        ),
        DeltaError::LootIdNotClientSafe { id } => write!(
            f,
            "loot row {id} is outside the Package loot range \
             {PACKAGE_LOOT_ID_FLOOR}..={PACKAGE_LOOT_ID_CEIL}; an inserted loot row must use an \
             identifier no client and no import can already own"
        ),
        DeltaError::CastIdNotClientSafe { id } => write!(
            f,
            "cast row {id} is outside the Package cast range \
             {PACKAGE_CAST_ID_FLOOR}..={PACKAGE_CAST_ID_CEIL}; an inserted cast row must use an \
             identifier no client and no import can already own"
        ),
        DeltaError::TrainerIdNotClientSafe { id } => write!(
            f,
            "trainer row {id} is outside the Package trainer range \
             {PACKAGE_TRAINER_ID_FLOOR}..={PACKAGE_TRAINER_ID_CEIL}; an inserted trainer row must \
             use an identifier no client and no import can already own"
        ),
        DeltaError::GossipIdNotClientSafe { id } => write!(
            f,
            "gossip row {id} is outside the Package gossip range \
             {PACKAGE_GOSSIP_ID_FLOOR}..={PACKAGE_GOSSIP_ID_CEIL}; an inserted gossip row must use \
             an identifier no client and no import can already own"
        ),
        DeltaError::GlobalsIdNotClientSafe { id } => write!(
            f,
            "globals row {id} is outside the Package globals range \
             {PACKAGE_GLOBALS_ID_FLOOR}..={PACKAGE_GLOBALS_ID_CEIL}; an inserted globals row must \
             use an identifier no client and no import can already own"
        ),
        DeltaError::SpellmetaIdNotClientSafe { id } => write!(
            f,
            "spell metadata row {id} is outside the Package spell metadata range \
             {PACKAGE_SPELLMETA_ID_FLOOR}..={PACKAGE_SPELLMETA_ID_CEIL}; an inserted spell \
             metadata row must use an identifier no client and no import can already own"
        ),
        DeltaError::CreatureIdNotClientSafe { id } => write!(
            f,
            "creature row {id} is outside the Package creature range \
             {PACKAGE_CREATURE_ID_FLOOR}..={PACKAGE_CREATURE_ID_CEIL}; an inserted creature \
             template or spawn must use an identifier no client and no import can already own"
        ),
        DeltaError::GameobjectIdNotClientSafe { id } => write!(
            f,
            "gameobject row {id} is outside the Package gameobject range \
             {PACKAGE_GAMEOBJECT_ID_FLOOR}..={PACKAGE_GAMEOBJECT_ID_CEIL}; an inserted gameobject \
             template, trap or spawn must use an identifier no client and no import can already own"
        ),
        DeltaError::CreatureAiIdNotClientSafe { id } => write!(
            f,
            "creature-ai row {id} is outside the Package EventAI range \
             {PACKAGE_CREATURE_AI_ID_FLOOR}..={PACKAGE_CREATURE_AI_ID_CEIL}; an inserted broadcast \
             text, summon placement or quest event requirement must use an identifier no client \
             and no import can already own"
        ),
        other => fmt_reserved_or_out_of_range(other, f),
    }
}

fn fmt_reserved_or_out_of_range(err: &DeltaError, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match err {
        DeltaError::SpellIdFixtureReserved { spell_id } => write!(
            f,
            "spell {spell_id} is fixture-reserved \
             ({FIXTURE_SPELL_ID_FLOOR}..={FIXTURE_SPELL_ID_CEIL} and \
             {FIXTURE_RESERVED_ID_FLOOR}..={FIXTURE_RESERVED_ID_CEIL}); no Package may claim it"
        ),
        DeltaError::EffectIndexOutOfRange { effect_index } => write!(
            f,
            "effect index {effect_index} is out of range; a spell has effects \
             0..={MAX_SPELL_EFFECT_INDEX}"
        ),
        DeltaError::ItemIdFixtureReserved { entry } => write!(
            f,
            "item {entry} is fixture-reserved \
             ({FIXTURE_RESERVED_ID_FLOOR}..={FIXTURE_RESERVED_ID_CEIL}); no Package may claim it"
        ),
        DeltaError::QuestIdFixtureReserved { entry } => write!(
            f,
            "quest {entry} is fixture-reserved \
             ({FIXTURE_RESERVED_ID_FLOOR}..={FIXTURE_RESERVED_ID_CEIL}); no Package may claim it"
        ),
        DeltaError::QuestObjectiveIndexOutOfRange { obj_index } => write!(
            f,
            "objective index {obj_index} is out of range; a quest has objectives \
             0..={MAX_QUEST_OBJECTIVE_INDEX}"
        ),
        DeltaError::QuestRewardChoiceIndexOutOfRange { choice_index } => write!(
            f,
            "reward choice index {choice_index} is out of range; a quest has reward choices \
             0..={MAX_QUEST_REWARD_CHOICE_INDEX}"
        ),
        DeltaError::LootIdFixtureReserved { id } => write!(
            f,
            "loot row {id} is fixture-reserved \
             ({FIXTURE_RESERVED_ID_FLOOR}..={FIXTURE_RESERVED_ID_CEIL}); no Package may claim it"
        ),
        DeltaError::CastIdFixtureReserved { id } => write!(
            f,
            "cast row {id} is fixture-reserved \
             ({FIXTURE_RESERVED_ID_FLOOR}..={FIXTURE_RESERVED_ID_CEIL}); no Package may claim it"
        ),
        DeltaError::TrainerIdFixtureReserved { id } => write!(
            f,
            "trainer row {id} is fixture-reserved \
             ({FIXTURE_RESERVED_ID_FLOOR}..={FIXTURE_RESERVED_ID_CEIL}); no Package may claim it"
        ),
        DeltaError::GossipIdFixtureReserved { id } => write!(
            f,
            "gossip row {id} is fixture-reserved \
             ({FIXTURE_RESERVED_ID_FLOOR}..={FIXTURE_RESERVED_ID_CEIL}); no Package may claim it"
        ),
        DeltaError::GlobalsIdFixtureReserved { id } => write!(
            f,
            "globals row {id} is fixture-reserved \
             ({FIXTURE_RESERVED_ID_FLOOR}..={FIXTURE_RESERVED_ID_CEIL}); no Package may claim it"
        ),
        DeltaError::SpellmetaIdFixtureReserved { id } => write!(
            f,
            "spell metadata row {id} is fixture-reserved \
             ({FIXTURE_RESERVED_ID_FLOOR}..={FIXTURE_RESERVED_ID_CEIL}); no Package may claim it"
        ),
        DeltaError::CreatureIdFixtureReserved { id } => write!(
            f,
            "creature row {id} is fixture-reserved \
             ({FIXTURE_CREATURE_ID_FLOOR}..={FIXTURE_CREATURE_ID_CEIL} for a template entry, \
             {FIXTURE_RESERVED_ID_FLOOR}..={FIXTURE_RESERVED_ID_CEIL} project-wide); no Package \
             may claim it"
        ),
        DeltaError::GameobjectIdFixtureReserved { id } => write!(
            f,
            "gameobject row {id} is fixture-reserved \
             ({FIXTURE_RESERVED_ID_FLOOR}..={FIXTURE_RESERVED_ID_CEIL}); no Package may claim it"
        ),
        DeltaError::CreatureAiIdFixtureReserved { id } => write!(
            f,
            "creature-ai row {id} is fixture-reserved \
             ({FIXTURE_RESERVED_ID_FLOOR}..={FIXTURE_RESERVED_ID_CEIL}); no Package may claim it"
        ),
        DeltaError::StatsLevelOutOfRange { level } => write!(
            f,
            "level {level} is outside 1..={MAX_STATS_LEVEL}; no stat curve carries a row for it"
        ),
        other => unreachable!("{other:?} is not an identifier-policy refusal"),
    }
}

fn fmt_general(err: &DeltaError, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match err {
        DeltaError::Malformed(detail) => write!(f, "not valid JSON: {detail}"),
        DeltaError::WrongArtifactKind { expected, found } => write!(
            f,
            "these bytes are {found}, not {expected}; one Package ships each artifact kind in \
             its own file and each is read by its own parser"
        ),
        DeltaError::UnsupportedVersion { found } => write!(
            f,
            "unsupported Package Delta version {found}; this build implements version {}",
            crate::DELTA_VERSION
        ),
        DeltaError::MissingMember { path } => write!(f, "missing member `{path}`"),
        DeltaError::UnexpectedMember { path, member } => {
            write!(f, "unexpected member `{member}` in `{path}`")
        }
        DeltaError::WrongJsonType { path, expected } => {
            write!(f, "member `{path}` must be {expected}")
        }
        DeltaError::InvalidPackageId { found } => write!(
            f,
            "`{found}` is not a Package identity: expected 1 to 64 characters of \
             a-z, 0-9, `-`, `_` or `.`"
        ),
        DeltaError::InvalidSourceHash { found } => write!(
            f,
            "`{found}` is not a source hash: expected 64 lowercase hexadecimal characters"
        ),
        DeltaError::UnknownTable { found } => write!(
            f,
            "unknown table `{found}`; a Package Delta claims {}",
            known_tables()
        ),
        DeltaError::UnknownOperation { found } => {
            write!(
                f,
                "unknown operation `{found}`; expected `insert` or `update`"
            )
        }
        DeltaError::DeleteNotSupported => f.write_str(
            "row deletion is not supported; override the row to inert with an `update` instead",
        ),
        DeltaError::InsertNotSupported { table } => write!(
            f,
            "`{table}` rows cannot be inserted: the primary key names an entity another Import \
             Family owns, which no Package may invent; an existing row may still be updated"
        ),
        DeltaError::MalformedKey { table, detail } => {
            write!(f, "malformed `{table}` key: {detail}")
        }
        DeltaError::UnknownField { table, field } => {
            write!(f, "`{table}` has no claimable column `{field}`")
        }
        DeltaError::UnknownFieldType { field, found } => {
            write!(f, "column `{field}` declares unknown type `{found}`")
        }
        DeltaError::FieldTypeMismatch {
            table,
            field,
            expected,
            found,
        } => write!(
            f,
            "`{table}`.`{field}` is `{expected}`, but the claim declares `{found}`"
        ),
        DeltaError::ValueOutOfRange { field, ty, literal } => {
            write!(f, "column `{field}`: `{literal}` does not fit `{ty}`")
        }
        DeltaError::KeyColumnClaimed { table, field } => write!(
            f,
            "`{table}`.`{field}` is part of the primary key; the claim's `key` already names it"
        ),
        DeltaError::IncompleteInsert { table, missing } => write!(
            f,
            "an `insert` into `{table}` must carry the whole row; {} column(s) absent: {}",
            missing.len(),
            missing.join(", ")
        ),
        DeltaError::EmptyUpdate { table } => {
            write!(f, "an `update` on `{table}` must claim at least one column")
        }
        DeltaError::DuplicateClaim { table, key } => write!(
            f,
            "`{table}` row {key} is claimed twice by one Package; merge the two claims"
        ),
        other => unreachable!("{other:?} is an identifier-policy refusal, not a general one"),
    }
}

impl std::error::Error for DeltaError {}

fn known_tables() -> String {
    prose_list(
        Table::ALL.iter().map(|table| format!("`{table}`")),
        "no table at all in this build",
    )
}

fn known_events() -> String {
    prose_list(
        HOOK_EVENT_NAMES.iter().map(|event| format!("`{event}`")),
        "no event at all in this build",
    )
}

fn prose_list(items: impl Iterator<Item = String>, empty: &str) -> String {
    let quoted: Vec<String> = items.collect();
    match quoted.split_last() {
        None => empty.to_owned(),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
    }
}
