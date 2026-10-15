use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

use crate::realm_core::RealmDb;

const DEFAULT_SAMPLE_SECS: u64 = 30;

pub(crate) fn sample_interval() -> Duration {
    let secs = std::env::var("LYRACORE_LOAD_SAMPLE_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_SAMPLE_SECS);
    Duration::from_secs(secs)
}

pub(crate) fn gateway_key(id: &str) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01B3;
    let mut hash = FNV_OFFSET_BASIS;
    for byte in id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

pub(crate) fn parse_db_ids(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for raw in text.split(['\n', ',', ';']) {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let (k, v) = (k.trim(), v.trim());
            if !k.is_empty() && !v.is_empty() {
                out.insert(k.to_string(), v.to_string());
            }
        }
    }
    out
}

#[derive(Clone, Debug, Default)]
struct Snapshot(BTreeMap<String, f64>);

const OCCUPANCY_FAMILY: &str = "spacetime_txn_cpu_time_sec_sum";

fn http_get(url: &str) -> Result<String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| anyhow!("only http:// URLs are supported (got {url:?})"))?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let addr = if hostport.contains(':') {
        hostport.to_string()
    } else {
        format!("{hostport}:80")
    };
    let mut s = TcpStream::connect(&addr).with_context(|| format!("connect metrics {addr}"))?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: {hostport}\r\nAccept: text/plain\r\nConnection: close\r\n\r\n"
    )?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).context("read metrics response")?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| anyhow!("malformed HTTP response from {url}"))?;
    let status = head.lines().next().unwrap_or_default();
    if !status.contains(" 200") {
        bail!("metrics endpoint {url} returned {status:?}");
    }
    Ok(body.to_string())
}

fn parse(text: &str) -> Snapshot {
    let mut m = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, val)) = line.rsplit_once(' ') else {
            continue;
        };
        let Ok(v) = val.trim().parse::<f64>() else {
            continue;
        };
        m.insert(key.to_string(), v);
    }
    Snapshot(m)
}

fn split_key(key: &str) -> (&str, &str) {
    match key.find('{') {
        Some(i) => (&key[..i], &key[i..]),
        None => (key, ""),
    }
}

fn db_filter(db_id: &str) -> String {
    format!("db=\"{db_id}")
}

impl Snapshot {
    fn sum(&self, name: &str, filter: &str) -> f64 {
        self.0
            .iter()
            .filter(|(k, _)| {
                let (n, labels) = split_key(k);
                n == name && labels.contains(filter)
            })
            .map(|(_, v)| *v)
            .sum()
    }
}

pub(crate) fn occupancy_pct(cpu_sec_delta: f64, wall_dt_secs: f64) -> f32 {
    if wall_dt_secs <= 0.0 || !cpu_sec_delta.is_finite() {
        return 0.0;
    }
    ((cpu_sec_delta / wall_dt_secs) * 100.0).max(0.0) as f32
}

pub(crate) struct OccupancySampler {
    metrics_url: String,
    db_ids: HashMap<String, String>,
    last: Mutex<Option<(Snapshot, Instant)>>,
}

impl OccupancySampler {
    pub(crate) fn from_env(stdb_base_url: &str) -> Self {
        let db_ids = parse_db_ids(&std::env::var("LYRACORE_METRICS_DB_IDS").unwrap_or_default());
        if db_ids.is_empty() {
            log::warn!(
                "LYRACORE_METRICS_DB_IDS is unset — SHARDLOAD will sample session counts but report \
                 writer occupancy as unmeasured for every shard. Discover each shard's \
                 database identity with `\"$(bash scripts/wire-harness.sh --bin vanilla-wire-bench)\" --dry-run 1`, \
                 then set LYRACORE_METRICS_DB_IDS=\"<shard>=<hex>,...\"."
            );
        }
        Self {
            metrics_url: format!("{}/v1/metrics", stdb_base_url.trim_end_matches('/')),
            db_ids,
            last: Mutex::new(None),
        }
    }

    pub(crate) fn sample_all(&self) -> HashMap<String, f32> {
        if self.db_ids.is_empty() {
            return HashMap::new();
        }
        let snap = match http_get(&self.metrics_url).map(|t| parse(&t)) {
            Ok(s) => s,
            Err(e) => {
                log::warn!(
                    "SHARDLOAD: metrics scrape failed ({e:#}) — occupancy unmeasured this cycle"
                );
                return HashMap::new();
            }
        };
        let now = Instant::now();
        let mut guard = self.last.lock().unwrap();
        let out = match &*guard {
            Some((prev_snap, prev_at)) => {
                let dt = now.duration_since(*prev_at).as_secs_f64();
                self.db_ids
                    .iter()
                    .map(|(shard, id)| {
                        let filter = db_filter(id);
                        let delta = snap.sum(OCCUPANCY_FAMILY, &filter)
                            - prev_snap.sum(OCCUPANCY_FAMILY, &filter);
                        (shard.clone(), occupancy_pct(delta, dt))
                    })
                    .collect()
            }
            None => HashMap::new(),
        };
        *guard = Some((snap, now));
        out
    }
}

pub(crate) fn sample_and_record<D: RealmDb>(
    db: &D,
    occupancy_by_shard: &HashMap<String, f32>,
    this_gateway_key: u64,
) -> Vec<String> {
    let rc = match db.realm_core() {
        Ok(rc) => rc,
        Err(e) => {
            return vec![format!(
                "SHARDLOAD: realm-core unreachable this cycle ({e:#}) — skipped"
            )]
        }
    };
    let mut shards = db.world_shards();
    if !shards.iter().any(|(name, _)| name == rc.shard_name()) {
        shards.push((rc.shard_name().to_string(), rc.clone()));
    }
    let mut lines = Vec::with_capacity(shards.len());
    for (name, shard) in shards {
        let sessions = shard.session_count() as u32;
        match occupancy_by_shard.get(&name) {
            Some(&pct) => {
                if let Err(e) = rc.record_shard_load(&name, pct, sessions, this_gateway_key) {
                    log::warn!("SHARDLOAD: record_shard_load({name}) failed: {e:#}");
                }
                lines.push(format!(
                    "SHARDLOAD shard={name} occupancy={pct:.1}% sessions={sessions}"
                ));
            }
            None => {
                lines.push(format!(
                    "SHARDLOAD shard={name} occupancy=unmeasured sessions={sessions} \
                     (set LYRACORE_METRICS_DB_IDS to enable occupancy)"
                ));
            }
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
# HELP spacetime_txn_cpu_time_sec_sum the writer's busy time
spacetime_txn_cpu_time_sec_sum{db="abc123",txn_type="Reducer"} 10.0
spacetime_txn_cpu_time_sec_sum{db="abc123",txn_type="Subscribe"} 2.0
spacetime_txn_cpu_time_sec_sum{db="zzz999",txn_type="Reducer"} 99.0
"#;

    #[test]
    fn parse_reads_labelled_samples_and_skips_comments() {
        let s = parse(SAMPLE);
        assert_eq!(s.0.len(), 3);
    }

    #[test]
    fn sum_filters_by_db_label_and_aggregates_every_txn_type() {
        let s = parse(SAMPLE);
        assert_eq!(
            s.sum(OCCUPANCY_FAMILY, &db_filter("abc123")),
            12.0,
            "reducer + subscribe CPU"
        );
        assert_eq!(s.sum(OCCUPANCY_FAMILY, &db_filter("zzz999")), 99.0);
        assert_eq!(s.sum(OCCUPANCY_FAMILY, &db_filter("nonexistent")), 0.0);
    }

    #[test]
    fn occupancy_pct_is_cpu_delta_over_wall_delta_times_100() {
        assert_eq!(occupancy_pct(15.0, 30.0), 50.0);
        assert_eq!(occupancy_pct(30.0, 30.0), 100.0);
        assert_eq!(occupancy_pct(0.0, 30.0), 0.0);
    }

    #[test]
    fn occupancy_pct_does_not_clamp_a_brief_overrun_to_100() {
        assert_eq!(occupancy_pct(33.0, 30.0), 110.0);
    }

    #[test]
    fn occupancy_pct_floors_a_negative_delta_at_zero_instead_of_going_negative() {
        assert_eq!(occupancy_pct(-5.0, 30.0), 0.0);
    }

    #[test]
    fn occupancy_pct_is_zero_for_a_non_positive_or_non_finite_input() {
        assert_eq!(
            occupancy_pct(15.0, 0.0),
            0.0,
            "no wall-clock delta means nothing to divide by"
        );
        assert_eq!(occupancy_pct(15.0, -1.0), 0.0);
        assert_eq!(occupancy_pct(f64::NAN, 30.0), 0.0);
        assert_eq!(occupancy_pct(f64::INFINITY, 30.0), 0.0);
    }

    #[test]
    fn parse_db_ids_reads_key_value_pairs_and_skips_comments_and_blanks() {
        let m = parse_db_ids(
            "lyracore=abc123\n\
             # a comment line\n\
             lyracore-world-1 = def456 ; lyracore-world-2=ghi789\n\
             \n\
             malformed-no-equals\n",
        );
        assert_eq!(m.len(), 3);
        assert_eq!(m.get("lyracore"), Some(&"abc123".to_string()));
        assert_eq!(
            m.get("lyracore-world-1"),
            Some(&"def456".to_string()),
            "whitespace around = is trimmed"
        );
        assert_eq!(
            m.get("lyracore-world-2"),
            Some(&"ghi789".to_string()),
            "; also separates entries"
        );
    }

    #[test]
    fn gateway_key_is_deterministic_and_distinguishes_different_ids() {
        assert_eq!(gateway_key("host-a:8085"), gateway_key("host-a:8085"));
        assert_ne!(
            gateway_key("host-a:8085"),
            gateway_key("host-b:8085"),
            "two different gateway identities must not collide onto the same ring key"
        );
        assert_ne!(
            gateway_key(""),
            0,
            "an empty identity must not hash to the same 0 pre-migration rows default to"
        );
    }

    #[test]
    fn parse_db_ids_of_an_empty_string_is_empty() {
        assert!(parse_db_ids("").is_empty());
        assert!(parse_db_ids("   \n # just a comment\n").is_empty());
    }

    use crate::realm_core::fake::{realm, Handle};

    const WORLD: &str = "world";
    const CORE: &str = "lyracore-realm";

    fn one_shard_realm() -> Handle {
        realm(&[WORLD, CORE], "", Some(CORE))
    }

    #[test]
    fn sample_and_record_forwards_occupancy_and_sessions_to_realm_core() {
        let h = one_shard_realm();
        *h.db_at(WORLD).open_sessions.lock().unwrap() = 7;
        let occ = HashMap::from([(WORLD.to_string(), 42.5f32)]);

        let lines = sample_and_record(&h, &occ, 99);

        assert_eq!(
            h.db_at(CORE).recorded_shard_loads.lock().unwrap().clone(),
            vec![(WORLD.to_string(), 42.5, 7, 99)],
            "the world shard's occupancy + session sample must land on REALM-CORE, not on itself, \
             carrying THIS gateway's key"
        );
        assert!(
            lines.iter().any(|l| l.starts_with("SHARDLOAD")
                && l.contains("occupancy=42.5%")
                && l.contains("sessions=7")),
            "a SHARDLOAD line must be emitted for the cycle: {lines:?}"
        );
    }

    #[test]
    fn sample_and_record_marks_a_shard_with_no_occupancy_entry_as_unmeasured_not_zero() {
        let h = one_shard_realm();
        *h.db_at(WORLD).open_sessions.lock().unwrap() = 3;
        let occ = HashMap::new();

        let lines = sample_and_record(&h, &occ, 99);

        assert!(
            h.db_at(CORE)
                .recorded_shard_loads
                .lock()
                .unwrap()
                .is_empty(),
            "an unmeasured shard must not write a fabricated occupancy row"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("occupancy=unmeasured") && l.contains("sessions=3")),
            "the line must say UNMEASURED, never a silent 0%: {lines:?}"
        );
    }

    #[test]
    fn sample_and_record_samples_every_connected_world_shard_not_just_the_first() {
        let h = realm(&["world-a", "world-b", CORE], "1:*=world-b", Some(CORE));
        *h.db_at("world-a").open_sessions.lock().unwrap() = 10;
        *h.db_at("world-b").open_sessions.lock().unwrap() = 20;
        let occ = HashMap::from([
            ("world-a".to_string(), 10.0f32),
            ("world-b".to_string(), 90.0f32),
        ]);

        sample_and_record(&h, &occ, 1);

        let mut recorded = h.db_at(CORE).recorded_shard_loads.lock().unwrap().clone();
        recorded.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            recorded,
            vec![
                ("world-a".to_string(), 10.0, 10, 1),
                ("world-b".to_string(), 90.0, 20, 1),
            ],
            "both world shards must be sampled, each under its OWN name and numbers, all under \
             this gateway's OWN key"
        );
    }

    #[test]
    fn sample_and_record_skips_the_cycle_cleanly_when_realm_core_is_unreachable() {
        use crate::realm_core::fake::realm_with_dead_core;
        let h = realm_with_dead_core(&[WORLD, CORE], "", CORE);
        let lines = sample_and_record(&h, &HashMap::from([(WORLD.to_string(), 5.0f32)]), 1);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("realm-core unreachable"), "{lines:?}");
    }

    #[test]
    fn sample_and_record_includes_realm_core_and_the_instance_pool() {
        let h = realm(
            &["world-a", "world-b", "instances", CORE],
            "1:*=world-b,36:*=instances",
            Some(CORE),
        );
        *h.db_at("world-a").open_sessions.lock().unwrap() = 10;
        *h.db_at("world-b").open_sessions.lock().unwrap() = 20;
        *h.db_at("instances").open_sessions.lock().unwrap() = 5;
        *h.db_at(CORE).open_sessions.lock().unwrap() = 35;
        let occupancy = HashMap::from([
            ("world-a".to_string(), 10.0),
            ("world-b".to_string(), 90.0),
            ("instances".to_string(), 5.0),
            (CORE.to_string(), 2.5),
        ]);

        let lines = sample_and_record(&h, &occupancy, 99);

        let mut recorded = h.db_at(CORE).recorded_shard_loads.lock().unwrap().clone();
        recorded.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            recorded,
            vec![
                ("instances".to_string(), 5.0, 5, 99),
                (CORE.to_string(), 2.5, 35, 99),
                ("world-a".to_string(), 10.0, 10, 99),
                ("world-b".to_string(), 90.0, 20, 99),
            ]
        );
        assert_eq!(lines.len(), 4);
        assert!(lines.contains(&format!(
            "SHARDLOAD shard={CORE} occupancy=2.5% sessions=35"
        )));
    }

    #[test]
    fn sample_and_record_reports_missing_realm_core_occupancy_without_a_false_sample() {
        let h = one_shard_realm();
        let occupancy = HashMap::from([(WORLD.to_string(), 42.5)]);

        let lines = sample_and_record(&h, &occupancy, 99);

        assert!(
            lines
                .iter()
                .any(|line| line
                    .starts_with(&format!("SHARDLOAD shard={CORE} occupancy=unmeasured ")))
        );
        let recorded = h.db_at(CORE).recorded_shard_loads.lock().unwrap().clone();
        assert_eq!(recorded, vec![(WORLD.to_string(), 42.5, 0, 99)]);
    }

    #[test]
    fn an_unconfigured_single_database_gateway_samples_and_records_onto_itself() {
        let h = realm(&[WORLD], "", None);
        *h.db_at(WORLD).open_sessions.lock().unwrap() = 4;
        sample_and_record(&h, &HashMap::from([(WORLD.to_string(), 33.0f32)]), 1);
        assert_eq!(
            h.db_at(WORLD).recorded_shard_loads.lock().unwrap().clone(),
            vec![(WORLD.to_string(), 33.0, 4, 1)]
        );
    }
}
