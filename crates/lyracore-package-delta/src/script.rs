use core::fmt;

use serde_json::Value;

use crate::canonical::write_json_string;
use crate::delta::{expect_members, member, object, string, PackageId, SourceHash};
use crate::error::{DeltaError, ScriptRefusal};
use crate::ids::is_package_script_id;

pub const SCRIPT_FAMILY: &str = "script";

pub const SCRIPT_ARTIFACT_KIND: &str = "script";

pub const SCRIPT_VERSION: u64 = 1;

include!("../../../datascripts/runtime-scripts/event-names.rs");

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactKind {
    // Version-1 Package Deltas omit kind for canonical-byte compatibility.
    Delta,
    Script,
    Other(String),
}

impl fmt::Display for ArtifactKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Delta => f.write_str("a Package Delta"),
            Self::Script => f.write_str("a Script Artifact"),
            Self::Other(kind) => write!(f, "an artifact of unknown kind `{kind}`"),
        }
    }
}

#[must_use]
pub fn artifact_kind(json: &str) -> Option<ArtifactKind> {
    let root: Value = serde_json::from_str(json).ok()?;
    match root.as_object()?.get("kind") {
        None => Some(ArtifactKind::Delta),
        Some(Value::String(kind)) if kind == SCRIPT_ARTIFACT_KIND => Some(ArtifactKind::Script),
        Some(Value::String(kind)) => Some(ArtifactKind::Other(kind.clone())),
        Some(_) => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScriptName(String);

impl ScriptName {
    pub fn new(name: impl Into<String>) -> Result<Self, DeltaError> {
        let name = name.into();
        let shaped = !name.is_empty()
            && name.len() <= 64
            && name.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.')
            });
        if shaped {
            Ok(Self(name))
        } else {
            Err(DeltaError::Script(ScriptRefusal::InvalidName {
                found: name,
            }))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ScriptName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventBinding(String);

impl EventBinding {
    pub fn parse(name: &str, package: &PackageId) -> Result<Self, DeltaError> {
        let known = HOOK_EVENT_NAMES.contains(&name)
            || name
                .strip_prefix(package.as_str())
                .and_then(|rest| rest.strip_prefix('.'))
                .is_some_and(is_package_event_name);
        if known {
            Ok(Self(name.to_owned()))
        } else {
            Err(DeltaError::Script(ScriptRefusal::UnknownEvent {
                found: name.to_owned(),
                package: package.as_str().to_owned(),
            }))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EventBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn is_package_event_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    script_id: u32,
    name: ScriptName,
    event: EventBinding,
    priority: i32,
    enabled: bool,
    source: String,
}

impl Script {
    pub fn new(
        script_id: u32,
        name: ScriptName,
        event: EventBinding,
        priority: i32,
        enabled: bool,
        source: impl Into<String>,
    ) -> Result<Self, DeltaError> {
        if !is_package_script_id(script_id) {
            return Err(DeltaError::Script(ScriptRefusal::IdNotInPackageBand {
                script_id,
            }));
        }
        let source = source.into();
        if source.trim().is_empty() {
            return Err(DeltaError::Script(ScriptRefusal::EmptySource {
                name: name.0.clone(),
            }));
        }
        Ok(Self {
            script_id,
            name,
            event,
            priority,
            enabled,
            source,
        })
    }

    #[must_use]
    pub const fn script_id(&self) -> u32 {
        self.script_id
    }

    #[must_use]
    pub const fn name(&self) -> &ScriptName {
        &self.name
    }

    #[must_use]
    pub const fn event(&self) -> &EventBinding {
        &self.event
    }

    #[must_use]
    pub const fn priority(&self) -> i32 {
        self.priority
    }

    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptArtifact {
    package: PackageId,
    source_hash: SourceHash,
    scripts: Vec<Script>,
}

impl ScriptArtifact {
    pub fn new(
        package: PackageId,
        source_hash: SourceHash,
        mut scripts: Vec<Script>,
    ) -> Result<Self, DeltaError> {
        scripts.sort_by_key(Script::script_id);
        for pair in scripts.windows(2) {
            if pair[0].script_id == pair[1].script_id {
                return Err(DeltaError::Script(ScriptRefusal::DuplicateId {
                    script_id: pair[0].script_id,
                }));
            }
        }

        let mut by_name: Vec<&ScriptName> = scripts.iter().map(Script::name).collect();
        by_name.sort();
        for pair in by_name.windows(2) {
            if pair[0] == pair[1] {
                return Err(DeltaError::Script(ScriptRefusal::DuplicateName {
                    name: pair[0].0.clone(),
                }));
            }
        }

        Ok(Self {
            package,
            source_hash,
            scripts,
        })
    }

    pub fn parse(json: &str) -> Result<Self, DeltaError> {
        let root: Value =
            serde_json::from_str(json).map_err(|e| DeltaError::Malformed(e.to_string()))?;
        let root = object(&root, "")?;

        match root.get("kind") {
            Some(Value::String(kind)) if kind == SCRIPT_ARTIFACT_KIND => {}
            Some(Value::String(kind)) => {
                return Err(DeltaError::WrongArtifactKind {
                    expected: "a Script Artifact",
                    found: ArtifactKind::Other(kind.clone()).to_string(),
                })
            }
            Some(_) => {
                return Err(DeltaError::WrongJsonType {
                    path: "kind".to_owned(),
                    expected: "a string",
                })
            }
            None => {
                return Err(DeltaError::WrongArtifactKind {
                    expected: "a Script Artifact",
                    found: ArtifactKind::Delta.to_string(),
                })
            }
        }

        expect_members(
            root,
            "",
            &["kind", "package", "scripts", "source_hash", "version"],
        )?;

        let version = member(root, "", "version")?;
        let version = version.as_u64().ok_or(DeltaError::WrongJsonType {
            path: "version".to_owned(),
            expected: "a version number",
        })?;
        if version != SCRIPT_VERSION {
            return Err(DeltaError::UnsupportedVersion {
                found: version.to_string(),
            });
        }

        let package = PackageId::new(string(member(root, "", "package")?, "package")?)?;
        let source_hash =
            SourceHash::new(string(member(root, "", "source_hash")?, "source_hash")?)?;

        let scripts = member(root, "", "scripts")?
            .as_array()
            .ok_or(DeltaError::WrongJsonType {
                path: "scripts".to_owned(),
                expected: "an array of scripts",
            })?;
        let scripts = scripts
            .iter()
            .enumerate()
            .map(|(index, script)| parse_script(script, index, &package))
            .collect::<Result<Vec<_>, _>>()?;

        Self::new(package, source_hash, scripts)
    }

    #[must_use]
    pub fn to_canonical_json(&self) -> String {
        let mut out = String::new();
        out.push_str("{\"kind\":");
        write_json_string(&mut out, SCRIPT_ARTIFACT_KIND);
        out.push_str(",\"version\":");
        out.push_str(&SCRIPT_VERSION.to_string());
        out.push_str(",\"package\":");
        write_json_string(&mut out, self.package.as_str());
        out.push_str(",\"source_hash\":");
        write_json_string(&mut out, self.source_hash.as_str());
        out.push_str(",\"scripts\":[");
        for (index, script) in self.scripts.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str("{\"script_id\":");
            out.push_str(&script.script_id.to_string());
            out.push_str(",\"name\":");
            write_json_string(&mut out, script.name.as_str());
            out.push_str(",\"event\":");
            write_json_string(&mut out, script.event.as_str());
            out.push_str(",\"priority\":");
            out.push_str(&script.priority.to_string());
            out.push_str(",\"enabled\":");
            out.push_str(if script.enabled { "true" } else { "false" });
            out.push_str(",\"source\":");
            write_json_string(&mut out, &script.source);
            out.push('}');
        }
        out.push_str("]}");
        out
    }

    #[must_use]
    pub const fn package(&self) -> &PackageId {
        &self.package
    }

    #[must_use]
    pub const fn source_hash(&self) -> &SourceHash {
        &self.source_hash
    }

    #[must_use]
    pub fn scripts(&self) -> &[Script] {
        &self.scripts
    }
}

fn parse_script(value: &Value, index: usize, package: &PackageId) -> Result<Script, DeltaError> {
    let path = format!("scripts[{index}]");
    let script = object(value, &path)?;
    expect_members(
        script,
        &path,
        &[
            "enabled",
            "event",
            "name",
            "priority",
            "script_id",
            "source",
        ],
    )?;

    let script_id = member(script, &path, "script_id")?;
    let script_id = script_id
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or(DeltaError::WrongJsonType {
            path: format!("{path}.script_id"),
            expected: "a whole number in 0..=4294967295",
        })?;

    let name = ScriptName::new(string(
        member(script, &path, "name")?,
        &format!("{path}.name"),
    )?)?;
    let event = EventBinding::parse(
        string(member(script, &path, "event")?, &format!("{path}.event"))?,
        package,
    )?;

    let priority = member(script, &path, "priority")?;
    let priority = priority
        .as_i64()
        .and_then(|n| i32::try_from(n).ok())
        .ok_or(DeltaError::WrongJsonType {
            path: format!("{path}.priority"),
            expected: "a whole number in -2147483648..=2147483647",
        })?;

    let enabled = member(script, &path, "enabled")?
        .as_bool()
        .ok_or(DeltaError::WrongJsonType {
            path: format!("{path}.enabled"),
            expected: "a boolean",
        })?;

    let source = string(member(script, &path, "source")?, &format!("{path}.source"))?;

    Script::new(script_id, name, event, priority, enabled, source)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TracedScript {
    pub script: Script,
    pub package: PackageId,
    pub source_hash: SourceHash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptConflict {
    Package {
        package: PackageId,
    },
    Id {
        script_id: u32,
        first: PackageId,
        second: PackageId,
    },
    Name {
        name: ScriptName,
        first: PackageId,
        second: PackageId,
    },
}

impl fmt::Display for ScriptConflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Package { package } => {
                write!(
                    f,
                    "package `{package}` appears in more than one Script Artifact"
                )
            }
            Self::Id {
                script_id,
                first,
                second,
            } => write!(
                f,
                "script {script_id} is shipped by both `{first}` and `{second}`"
            ),
            Self::Name {
                name,
                first,
                second,
            } => write!(
                f,
                "Runtime Script name `{name}` is shipped by both `{first}` and `{second}`"
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScriptTrace {
    scripts: Vec<TracedScript>,
    conflicts: Vec<ScriptConflict>,
}

impl ScriptTrace {
    #[must_use]
    pub fn scripts(&self) -> &[TracedScript] {
        &self.scripts
    }

    #[must_use]
    pub fn conflicts(&self) -> &[ScriptConflict] {
        &self.conflicts
    }

    #[must_use]
    pub fn is_clear(&self) -> bool {
        self.conflicts.is_empty()
    }
}

#[must_use]
pub fn trace_scripts(artifacts: &[ScriptArtifact]) -> ScriptTrace {
    let mut trace = ScriptTrace::default();
    let mut packages: Vec<PackageId> = Vec::new();
    for artifact in artifacts {
        if packages.contains(&artifact.package) {
            trace.conflicts.push(ScriptConflict::Package {
                package: artifact.package.clone(),
            });
            continue;
        }
        packages.push(artifact.package.clone());
        for script in &artifact.scripts {
            if let Some(seen) = trace
                .scripts
                .iter()
                .find(|traced| traced.script.script_id == script.script_id)
            {
                trace.conflicts.push(ScriptConflict::Id {
                    script_id: script.script_id,
                    first: seen.package.clone(),
                    second: artifact.package.clone(),
                });
                continue;
            }
            if let Some(seen) = trace
                .scripts
                .iter()
                .find(|traced| traced.script.name == script.name)
            {
                trace.conflicts.push(ScriptConflict::Name {
                    name: script.name.clone(),
                    first: seen.package.clone(),
                    second: artifact.package.clone(),
                });
                continue;
            }
            trace.scripts.push(TracedScript {
                script: script.clone(),
                package: artifact.package.clone(),
                source_hash: artifact.source_hash.clone(),
            });
        }
    }
    trace.scripts.sort_by_key(|traced| traced.script.script_id);
    trace
}
