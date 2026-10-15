use std::collections::BTreeMap;
use std::fmt;

use crate::delta::{Operation, PackageDelta, PackageId, PrimaryKey};
use crate::schema::{FieldValue, Table};

#[derive(Debug, Clone, PartialEq)]
pub struct ClaimedField {
    pub value: FieldValue,
    pub claimed_by: PackageId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TracedRow {
    key: PrimaryKey,
    operation: Operation,
    fields: BTreeMap<String, ClaimedField>,
}

impl TracedRow {
    #[must_use]
    pub const fn key(&self) -> PrimaryKey {
        self.key
    }

    #[must_use]
    pub const fn table(&self) -> Table {
        self.key.table()
    }

    #[must_use]
    pub const fn row_id(&self) -> u64 {
        self.key.row_id()
    }

    #[must_use]
    pub const fn operation(&self) -> Operation {
        self.operation
    }

    #[must_use]
    pub const fn fields(&self) -> &BTreeMap<String, ClaimedField> {
        &self.fields
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClaimConflict {
    Column {
        key: PrimaryKey,
        field: String,
        holder: PackageId,
        holder_value: FieldValue,
        challenger: PackageId,
        challenger_value: FieldValue,
    },
    InsertedRow {
        key: PrimaryKey,
        holder: PackageId,
        challenger: PackageId,
    },
}

impl ClaimConflict {
    #[must_use]
    pub const fn key(&self) -> PrimaryKey {
        match self {
            Self::Column { key, .. } | Self::InsertedRow { key, .. } => *key,
        }
    }
}

impl fmt::Display for ClaimConflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Column {
                key,
                field,
                holder,
                holder_value,
                challenger,
                challenger_value,
            } => write!(
                f,
                "family `{}`, table `{}`, row {key}: packages `{holder}` and `{challenger}` both claim column \
                 `{field}` (`{holder}` sets {holder_value}, `{challenger}` sets {challenger_value})",
                key.table().family(),
                key.table()
            ),
            Self::InsertedRow {
                key,
                holder,
                challenger,
            } => write!(
                f,
                "family `{}`, table `{}`, row {key}: packages `{holder}` and `{challenger}` both insert this row",
                key.table().family(),
                key.table()
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClaimTrace {
    rows: Vec<TracedRow>,
    conflicts: Vec<ClaimConflict>,
}

impl ClaimTrace {
    #[must_use]
    pub fn rows(&self) -> &[TracedRow] {
        &self.rows
    }

    #[must_use]
    pub fn conflicts(&self) -> &[ClaimConflict] {
        &self.conflicts
    }

    #[must_use]
    pub fn is_clear(&self) -> bool {
        self.conflicts.is_empty()
    }
}

#[must_use]
pub fn trace(deltas: &[PackageDelta]) -> ClaimTrace {
    let mut rows: BTreeMap<PrimaryKey, TracedRow> = BTreeMap::new();
    let mut inserted_by: BTreeMap<PrimaryKey, PackageId> = BTreeMap::new();
    let mut conflicts = Vec::new();

    for delta in deltas {
        let package = delta.package();
        for claim in delta.claims() {
            let key = claim.key();

            if claim.operation() == Operation::Insert {
                match inserted_by.get(&key) {
                    Some(holder) => conflicts.push(ClaimConflict::InsertedRow {
                        key,
                        holder: holder.clone(),
                        challenger: package.clone(),
                    }),
                    None => {
                        inserted_by.insert(key, package.clone());
                    }
                }
            }

            let row = rows.entry(key).or_insert_with(|| TracedRow {
                key,
                operation: Operation::Update,
                fields: BTreeMap::new(),
            });
            if claim.operation() == Operation::Insert {
                row.operation = Operation::Insert;
            }

            for (name, value) in claim.fields() {
                match row.fields.get(name) {
                    Some(held) => conflicts.push(ClaimConflict::Column {
                        key,
                        field: name.clone(),
                        holder: held.claimed_by.clone(),
                        holder_value: held.value.clone(),
                        challenger: package.clone(),
                        challenger_value: value.clone(),
                    }),
                    None => {
                        row.fields.insert(
                            name.clone(),
                            ClaimedField {
                                value: value.clone(),
                                claimed_by: package.clone(),
                            },
                        );
                    }
                }
            }
        }
    }

    conflicts.sort_by(|a, b| conflict_order(a).cmp(&conflict_order(b)));

    ClaimTrace {
        rows: rows.into_values().collect(),
        conflicts,
    }
}

fn conflict_order(conflict: &ClaimConflict) -> (PrimaryKey, &str, &str, &str) {
    match conflict {
        ClaimConflict::InsertedRow {
            key,
            holder,
            challenger,
        } => (*key, "", holder.as_str(), challenger.as_str()),
        ClaimConflict::Column {
            key,
            field,
            holder,
            challenger,
            ..
        } => (*key, field.as_str(), holder.as_str(), challenger.as_str()),
    }
}
