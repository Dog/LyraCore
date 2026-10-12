use super::*;

#[test]
fn duel_winner_aoi_fan_excludes_the_two_private_recipients() {
    assert!(!duel_winner_audience(10, 10, 20));
    assert!(!duel_winner_audience(20, 10, 20));
    assert!(duel_winner_audience(30, 10, 20));
    assert!(!duel_winner_audience(0, 10, 20));
}
