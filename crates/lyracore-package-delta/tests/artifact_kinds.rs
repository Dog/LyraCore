use lyracore_package_delta::{artifact_kind, ArtifactKind, PackageDelta, ScriptArtifact};

const HASH_A: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

const DELTA: &str = concat!(
    r#"{"version":1,"package":"example.bolt","#,
    r#""source_hash":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","#,
    r#""claims":[{"table":"game_spell","key":{"spell_id":133},"operation":"update","#,
    r#""fields":{"cooldown_ms":{"type":"u32","value":1500}}}]}"#,
);

fn script_artifact() -> String {
    format!(
        r#"{{"kind":"script","version":1,"package":"example.bolt","source_hash":"{HASH_A}","scripts":[]}}"#
    )
}

#[test]
fn an_artifact_with_no_kind_member_is_a_package_delta() {
    assert_eq!(artifact_kind(DELTA), Some(ArtifactKind::Delta));
}

#[test]
fn an_artifact_naming_the_script_kind_is_a_script_artifact() {
    assert_eq!(
        artifact_kind(&script_artifact()),
        Some(ArtifactKind::Script)
    );
}

#[test]
fn an_unknown_kind_is_reported_by_name() {
    let json = r#"{"kind":"loot-table","version":1}"#;

    assert_eq!(
        artifact_kind(json),
        Some(ArtifactKind::Other("loot-table".to_owned()))
    );
}

#[test]
fn bytes_no_parser_can_read_have_no_kind() {
    for bytes in ["{ not even valid }", "[]", r#""a string""#, r#"{"kind":7}"#] {
        assert_eq!(artifact_kind(bytes), None, "{bytes}");
    }
}

#[test]
fn the_delta_parser_refuses_a_script_artifact_by_name() {
    let refusal = PackageDelta::parse(&script_artifact()).expect_err("the artifact is refused");

    let report = refusal.to_string();
    assert!(report.contains("a Script Artifact"), "{report}");
    assert!(report.contains("a Package Delta"), "{report}");
}

#[test]
fn the_script_parser_refuses_a_package_delta_by_name() {
    let refusal = ScriptArtifact::parse(DELTA).expect_err("the artifact is refused");

    let report = refusal.to_string();
    assert!(report.contains("a Package Delta"), "{report}");
    assert!(report.contains("a Script Artifact"), "{report}");
}

#[test]
fn both_parsers_refuse_a_kind_neither_of_them_reads() {
    let json = format!(
        r#"{{"kind":"loot-table","version":1,"package":"example.bolt","source_hash":"{HASH_A}","scripts":[]}}"#
    );

    let from_delta = PackageDelta::parse(&json).expect_err("the artifact is refused");
    let from_script = ScriptArtifact::parse(&json).expect_err("the artifact is refused");

    assert!(
        from_delta.to_string().contains("loot-table"),
        "{from_delta}"
    );
    assert!(
        from_script.to_string().contains("loot-table"),
        "{from_script}"
    );
}

#[test]
fn a_canonical_package_delta_still_reads_as_one() {
    let delta = PackageDelta::parse(DELTA).expect("artifact parses");

    let canonical = delta.to_canonical_json();

    assert_eq!(artifact_kind(&canonical), Some(ArtifactKind::Delta));
    assert_eq!(
        PackageDelta::parse(&canonical).expect("canonical bytes parse"),
        delta
    );
}

#[test]
fn a_canonical_script_artifact_still_reads_as_one() {
    let artifact = ScriptArtifact::parse(&script_artifact()).expect("artifact parses");

    let canonical = artifact.to_canonical_json();

    assert_eq!(artifact_kind(&canonical), Some(ArtifactKind::Script));
    assert_eq!(
        ScriptArtifact::parse(&canonical).expect("canonical bytes parse"),
        artifact
    );
}
