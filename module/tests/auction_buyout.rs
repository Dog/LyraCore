mod support;

use support::Standalone;

#[test]
#[ignore = "requires the SpacetimeDB 2.7.1 CLI and Wasm toolchain"]
fn real_realm_reducer_commits_exact_buyout_mail_before_the_next_transaction() {
    let mut standalone = Standalone::start("auction-buyout");
    standalone.publish_module();
    standalone.assert_call("claim_operator", &[]);
    standalone.assert_call("install_guid_range", &["0"]);
    standalone.assert_sql("DELETE FROM game_event_reaper_schedule");
    for (reducer, args) in [
        ("debug_stage_auction_buyout_fixture", &[][..]),
        (
            "realm_auction_decide_bid",
            &[
                "5090050",
                r#"{"guid":5090051,"ownership":null}"#,
                "5090050",
                "1",
                "900",
            ][..],
        ),
        (
            "realm_auction_decide_bid",
            &[
                "5090056",
                r#"{"guid":5090058,"ownership":null}"#,
                "5090056",
                "1",
                "60",
            ][..],
        ),
        ("debug_verify_auction_buyout_fixture", &[][..]),
    ] {
        standalone.assert_call(reducer, args);
    }
}
