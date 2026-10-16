#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented
    )
)]

use crate::accept::BlockingTaskCapacity;

#[derive(Clone, Debug)]
pub struct GatewayConfig {
    pub logon_bind: String,
    pub world_bind: String,
    pub stdb_uri: String,
    pub module_name: String,
    pub coordinator_token: Option<String>,
    pub gateway_id: String,
    pub(crate) blocking_task_capacity: BlockingTaskCapacity,
}

fn default_hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/proc/sys/kernel/hostname").ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "localhost".to_string())
}

impl GatewayConfig {
    pub fn from_env() -> Self {
        let get = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
        let world_bind = get("LYRACORE_WORLD_BIND", "0.0.0.0:8085");
        let default_gateway_id = format!(
            "{}:{}",
            default_hostname(),
            world_bind.rsplit(':').next().unwrap_or(&world_bind)
        );
        Self {
            logon_bind: get("LYRACORE_LOGON_BIND", "0.0.0.0:3724"),
            world_bind: world_bind.clone(),
            stdb_uri: get("LYRACORE_SPACETIMEDB_URL", "http://127.0.0.1:3000"),
            module_name: get("LYRACORE_DATABASE", "lyracore"),
            coordinator_token: std::env::var("LYRACORE_COORDINATOR_TOKEN").ok(),
            gateway_id: get("LYRACORE_GATEWAY_ID", &default_gateway_id),
            blocking_task_capacity: BlockingTaskCapacity::new(
                max_blocking_threads()
                    .saturating_sub(encoding_workers())
                    .max(1),
            ),
        }
    }
}

pub(crate) fn encoding_workers() -> usize {
    (max_blocking_threads() / 4).clamp(1, 8)
}

pub(crate) fn world_connection_limit() -> usize {
    std::env::var("LYRACORE_MAX_WORLD_CONNECTIONS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|limit| *limit > 0 && *limit <= tokio::sync::Semaphore::MAX_PERMITS)
        .unwrap_or(16_384)
}

pub const INSTANCE_BUCKETS: u64 = 8;

#[derive(Clone, Debug, PartialEq)]
struct ShardRule {
    map_id: Option<u32>,
    bucket: Option<u64>,
    db: String,
}

#[derive(Clone, Debug)]
pub struct ShardMap {
    default_db: String,
    rules: Vec<ShardRule>,
    realm_core: Option<String>,
}

impl ShardMap {
    pub fn from_env(default_db: &str) -> Self {
        let text = match std::env::var("LYRACORE_SHARD_MAP") {
            Ok(t) => t,
            Err(_) => match std::env::var("LYRACORE_SHARD_MAP_FILE") {
                Ok(path) => std::fs::read_to_string(&path).unwrap_or_else(|e| {
                    log::error!(
                        "LYRACORE_SHARD_MAP_FILE {path}: {e} — falling back to a single shard"
                    );
                    String::new()
                }),
                Err(_) => String::new(),
            },
        };
        Self::parse(default_db, &text)
            .with_realm_core(std::env::var("LYRACORE_REALM_CORE").ok().as_deref())
    }

    pub fn with_realm_core(mut self, name: Option<&str>) -> Self {
        self.realm_core = name
            .map(str::trim)
            .filter(|n| !n.is_empty() && *n != self.default_db)
            .map(str::to_string);
        self
    }

    pub fn realm_core_db(&self) -> Option<&str> {
        self.realm_core.as_deref()
    }

    pub fn auth_db(&self, connected: impl Fn(&str) -> bool) -> std::result::Result<&str, &str> {
        match self.realm_core.as_deref() {
            None => Ok(&self.default_db),
            Some(db) if connected(db) => Ok(db),
            Some(db) => Err(db),
        }
    }

    pub fn parse(default_db: &str, text: &str) -> Self {
        let mut rules = Vec::new();
        for raw in text.split(['\n', ',', ';']) {
            let rule = raw.split('#').next().unwrap_or("").trim();
            if rule.is_empty() {
                continue;
            }
            match parse_shard_rule(rule) {
                Some(r) => rules.push(r),
                None => log::error!(
                    "shard map: ignoring malformed rule {rule:?} (want `<map_id|*>:<bucket|*>=<db>`)"
                ),
            }
        }
        Self {
            default_db: default_db.to_string(),
            rules,
            realm_core: None,
        }
    }

    pub fn resolve(&self, map_id: u32, instance_id: u64) -> &str {
        let bucket = instance_id % INSTANCE_BUCKETS;
        self.rules
            .iter()
            .find(|r| r.map_id.is_none_or(|m| m == map_id) && r.bucket.is_none_or(|b| b == bucket))
            .map_or(self.default_db.as_str(), |r| r.db.as_str())
    }

    pub fn resolve_connected(
        &self,
        map_id: u32,
        instance_id: u64,
        connected: impl Fn(&str) -> bool,
    ) -> &str {
        let db = self.resolve(map_id, instance_id);
        if connected(db) {
            return db;
        }
        log::warn!(
            "shard {db} owns map {map_id}/instance {instance_id} but is not connected — falling \
             back to the default database {}",
            self.default_db
        );
        &self.default_db
    }

    pub fn default_db(&self) -> &str {
        &self.default_db
    }

    pub fn instance_pool(&self, map_id: u32) -> Vec<&str> {
        let mut pool: Vec<&str> = Vec::new();
        for bucket in 0..INSTANCE_BUCKETS {
            let db = self.resolve(map_id, bucket);
            if !pool.contains(&db) {
                pool.push(db);
            }
        }
        pool
    }

    /// Rebucket only an idle pool; new entrants can split a live Party across shards.
    pub fn instance_owner<'a>(
        &'a self,
        map_id: u32,
        instance_id: u64,
        holder: &'a str,
        connected: impl Fn(&str) -> bool,
    ) -> &'a str {
        if instance_id != 0 && connected(holder) && self.instance_pool(map_id).contains(&holder) {
            return holder;
        }
        self.resolve_connected(map_id, instance_id, connected)
    }

    pub fn shards(&self) -> Vec<String> {
        let mut dbs = vec![self.default_db.clone()];
        for r in &self.rules {
            if !dbs.contains(&r.db) {
                dbs.push(r.db.clone());
            }
        }
        dbs
    }

    pub fn check_instance_hosting(
        &self,
        imported_maps: &[u32],
        hosts_instances: impl Fn(&str) -> Option<bool>,
        connected: impl Fn(&str) -> bool,
    ) -> InstanceHosting {
        let hosts = |db: &str| hosts_instances(db).unwrap_or(true);
        let owners = lyracore_shared::instance::DUNGEON_MAPS
            .iter()
            .filter(|&&map_id| {
                map_id == 36
                    || imported_maps.contains(&map_id)
                    || self.rules.iter().any(|rule| rule.map_id == Some(map_id))
            })
            .flat_map(|&map_id| (0..INSTANCE_BUCKETS).map(move |bucket| (map_id, bucket)));
        let mut load_not_moved: Option<String> = None;
        for (map_id, instance_id) in owners {
            let resolved = self.resolve(map_id, instance_id);
            let degraded = !connected(resolved);
            let owner = if degraded {
                self.default_db.as_str()
            } else {
                resolved
            };
            if !hosts(owner) && degraded {
                return InstanceHosting::UnreachableHost(format!(
                    "instances of DUNGEON map {map_id} are routed to database {resolved}, which is \
                     NOT CONNECTED — so dungeon entries degrade back to {owner}, whose \
                     `game_config.hosts_instances` is FALSE. Until {resolved} is reachable every \
                     dungeon entry files a lease row and spawns NOTHING: players and bots walk into \
                     a completely empty instance. The open world is unaffected and the \
                     routing itself is correct, so the gateway is COMING UP ANYWAY — bring \
                     {resolved} up and restart the gateway to restore dungeons (the coordinator \
                     watchdog does not re-run this check, and it does not connect shards that were \
                     absent at startup)."
                ));
            }
            if !hosts(owner) {
                return InstanceHosting::NoHost(format!(
                    "instances of DUNGEON map {map_id} are owned by database {owner}, and \
                     {owner}'s `game_config.hosts_instances` is FALSE — every dungeon entry there \
                     files a lease row and spawns NOTHING, so players and bots walk into a \
                     completely empty instance and no other database will ever populate it \
                     (measured: 8 consecutive instances with 0 entities, 4 bot tests failing as if \
                     gameplay had regressed). Fix ONE of these and restart: (a) let that database \
                     host its own dungeons — `spacetime sql {owner} \"UPDATE game_config SET \
                     hosts_instances = true WHERE id = 0\"`; or (b) configure LYRACORE_SHARD_MAP so map \
                     {map_id}'s instances are routed to a database that DOES host populations, \
                     e.g. LYRACORE_SHARD_MAP=\"{map_id}:*=<instances-database>\". Note that EVERY \
                     instance bucket is checked, so a pool whose members disagree about the flag \
                     lands here on the one member that forgot it, not on all eight."
                ));
            }
            if owner != self.default_db && hosts(&self.default_db) && load_not_moved.is_none() {
                load_not_moved = Some(format!(
                    "instances of DUNGEON map {map_id} are owned by database {owner}, but the \
                     dungeon portal's areatrigger runs on {0} — and {0}'s \
                     `game_config.hosts_instances` is still TRUE, so every entry spawns the whole \
                     population (~207 creatures + 28 gameobject copies) on the world writer and \
                     evicts it again after the transfer. The run works, but the unused population wastes work. Fix: `spacetime sql \
                     {0} \"UPDATE game_config SET hosts_instances = false WHERE id = 0\"`.",
                    self.default_db
                ));
            }
        }
        match load_not_moved {
            Some(msg) => InstanceHosting::LoadNotMoved(msg),
            None => InstanceHosting::Consistent,
        }
    }

    pub fn databases(&self) -> Vec<String> {
        let mut dbs = self.shards();
        if let Some(rc) = &self.realm_core {
            if !dbs.contains(rc) {
                dbs.push(rc.clone());
            }
        }
        dbs
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum InstanceHosting {
    Consistent,
    LoadNotMoved(String),
    UnreachableHost(String),
    NoHost(String),
}

impl InstanceHosting {
    pub fn enforce(self) -> std::result::Result<(), String> {
        match self {
            Self::Consistent => Ok(()),
            Self::LoadNotMoved(msg) => {
                log::warn!("{msg}");
                Ok(())
            }
            Self::UnreachableHost(msg) => {
                log::error!("{msg}");
                Ok(())
            }
            Self::NoHost(msg) => Err(msg),
        }
    }
}

fn parse_shard_rule(rule: &str) -> Option<ShardRule> {
    let (loc, db) = rule.split_once('=')?;
    let db = db.trim();
    if db.is_empty() {
        return None;
    }
    let (map, bucket) = match loc.split_once(':') {
        Some((m, b)) => (m.trim(), b.trim()),
        None => (loc.trim(), "*"),
    };
    let map_id = if map == "*" {
        None
    } else {
        Some(map.parse::<u32>().ok()?)
    };
    let bucket = if bucket == "*" {
        None
    } else {
        Some(bucket.parse::<u64>().ok()?)
    };
    if bucket.is_some_and(|b| b >= INSTANCE_BUCKETS) {
        return None;
    }
    Some(ShardRule {
        map_id,
        bucket,
        db: db.to_string(),
    })
}

pub fn quest_log_fields_enabled() -> bool {
    std::env::var("LYRACORE_QUEST_LOG").map_or(true, |v| v != "0")
}

pub fn advertised_realm_address() -> Option<String> {
    realm_address_override(std::env::var("LYRACORE_REALM_ADDRESS").ok())
}

pub fn realm_address_override(raw: Option<String>) -> Option<String> {
    raw.map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn advertised_realm_address_or(row_address: String) -> String {
    advertised_realm_address().unwrap_or(row_address)
}

pub fn advertised_address_is_unreachable(advertised: &str, world_bind: &str) -> bool {
    let bind = host_of(world_bind);
    is_loopback_host(host_of(advertised)) && !bind.is_empty() && !is_loopback_host(bind)
}

fn host_of(address: &str) -> &str {
    let host = match address.rsplit_once(':') {
        Some((host, _)) if !host.is_empty() => host,
        _ => address,
    };
    host.trim_start_matches('[').trim_end_matches(']')
}

fn is_loopback_host(host: &str) -> bool {
    match host.parse::<std::net::IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        Err(_) => host.eq_ignore_ascii_case("localhost"),
    }
}

pub fn aoi_enabled() -> bool {
    std::env::var("LYRACORE_AOI").map_or(true, |v| v != "0")
}

pub fn call_pipes() -> usize {
    std::env::var("LYRACORE_CALL_PIPES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(4)
        .clamp(1, 16)
}

pub fn synthetic_owner_identity(account_id: u64) -> [u8; 32] {
    let mut id = [0u8; 32];
    id[..8].copy_from_slice(b"LYRAGWID");
    id[24..].copy_from_slice(&account_id.to_le_bytes());
    id
}

pub fn writer_trace_enabled() -> bool {
    std::env::var("LYRACORE_WRITER_TRACE").is_ok_and(|v| v == "1")
}

pub fn max_sessions() -> anyhow::Result<usize> {
    admission_limit("LYRACORE_MAX_SESSIONS")
}

pub fn admit_concurrency() -> anyhow::Result<usize> {
    admission_limit("LYRACORE_ADMIT_CONCURRENCY")
}

fn admission_limit(name: &str) -> anyhow::Result<usize> {
    parse_admission_limit(name, std::env::var_os(name).as_deref())
}

fn parse_admission_limit(name: &str, raw: Option<&std::ffi::OsStr>) -> anyhow::Result<usize> {
    let Some(raw) = raw else {
        return Ok(0);
    };
    raw.to_str()
.map(str::trim)
.filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
.and_then(|value| value.parse().ok())
.ok_or_else(|| {
            anyhow::anyhow!(
                "invalid {name}={raw:?}: expected a decimal integer from 0 to {}, with 0 for unlimited",
                usize::MAX
            )
        })
}

pub const DEFAULT_MAX_BLOCKING_THREADS: usize = 512;

pub fn max_blocking_threads() -> usize {
    max_blocking_threads_from_env(
        std::env::var("LYRACORE_MAX_BLOCKING_THREADS")
            .ok()
            .as_deref(),
    )
}

pub fn max_blocking_threads_from_env(raw: Option<&str>) -> usize {
    raw.map(str::trim)
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0 && n <= tokio::sync::Semaphore::MAX_PERMITS)
        .unwrap_or(DEFAULT_MAX_BLOCKING_THREADS)
}

#[cfg(test)]
mod max_blocking_threads_tests {
    use super::*;

    #[test]
    fn unset_is_tokios_own_default() {
        assert_eq!(max_blocking_threads_from_env(None), 512);
        assert_eq!(
            max_blocking_threads_from_env(None),
            DEFAULT_MAX_BLOCKING_THREADS
        );
    }

    #[test]
    fn a_plain_number_is_taken_as_written() {
        assert_eq!(max_blocking_threads_from_env(Some("4096")), 4096);
        assert_eq!(max_blocking_threads_from_env(Some("1")), 1);
        assert_eq!(max_blocking_threads_from_env(Some(" 2048 ")), 2048);
    }

    #[test]
    fn zero_falls_back_rather_than_seating_nobody() {
        assert_eq!(
            max_blocking_threads_from_env(Some("0")),
            DEFAULT_MAX_BLOCKING_THREADS
        );
    }

    #[test]
    fn a_malformed_value_falls_back_to_the_default() {
        for junk in ["", "   ", "-1", "lots", "4096x", "4_096", "1e3", "512.0"] {
            assert_eq!(
                max_blocking_threads_from_env(Some(junk)),
                DEFAULT_MAX_BLOCKING_THREADS,
                "{junk:?} should have fallen back to the default"
            );
        }
    }
}

#[cfg(test)]
mod realm_address_tests {
    use super::*;

    #[test]
    fn an_unset_override_leaves_the_game_realm_row_in_charge() {
        assert_eq!(realm_address_override(None), None);
    }

    #[test]
    fn a_blank_override_reads_as_unset_rather_than_advertising_nothing() {
        for blank in ["", "   ", "\t\n"] {
            assert_eq!(
                realm_address_override(Some(blank.to_string())),
                None,
                "{blank:?} must read as unconfigured"
            );
        }
    }

    #[test]
    fn a_configured_address_wins_over_the_row_and_is_trimmed() {
        assert_eq!(
            realm_address_override(Some("192.168.1.50:8085".to_string())),
            Some("192.168.1.50:8085".to_string())
        );
        assert_eq!(
            realm_address_override(Some("  192.168.1.50:8085\n".to_string())),
            Some("192.168.1.50:8085".to_string())
        );
    }

    #[test]
    fn advertising_loopback_while_listening_for_remote_clients_is_unreachable() {
        for bind in ["0.0.0.0:8085", "159.69.88.70:8085", "192.168.1.50:8085"] {
            assert!(
                advertised_address_is_unreachable("127.0.0.1:8085", bind),
                "loopback advertised against {bind} must be reported"
            );
        }
    }

    #[test]
    fn a_loopback_fixture_is_never_reported() {
        for bind in ["127.0.0.1:8085", "localhost:8085", "[::1]:8085"] {
            assert!(
                !advertised_address_is_unreachable("127.0.0.1:8085", bind),
                "the loopback fixture bound to {bind} must stay silent"
            );
        }
    }

    #[test]
    fn a_routable_advertised_address_is_never_reported_whatever_the_bind() {
        for bind in ["0.0.0.0:8085", "127.0.0.1:8085", "10.0.0.5:8085"] {
            assert!(!advertised_address_is_unreachable(
                "159.69.88.70:8085",
                bind
            ));
            assert!(!advertised_address_is_unreachable(
                "realm.example.com:8085",
                bind
            ));
        }
    }

    #[test]
    fn every_spelling_of_loopback_counts_as_loopback() {
        for advertised in [
            "127.0.0.1:8085",
            "127.1.2.3:8085",
            "localhost:8085",
            "LOCALHOST:8085",
            "[::1]:8085",
        ] {
            assert!(
                advertised_address_is_unreachable(advertised, "0.0.0.0:8085"),
                "{advertised} is loopback"
            );
        }
    }

    #[test]
    fn a_host_that_only_looks_like_loopback_is_left_alone() {
        assert!(!advertised_address_is_unreachable(
            "127.example.com:8085",
            "0.0.0.0:8085"
        ));
        assert!(!advertised_address_is_unreachable(
            "localhost.example.com:8085",
            "0.0.0.0:8085"
        ));
    }

    #[test]
    fn malformed_input_stays_silent_instead_of_panicking() {
        for (advertised, bind) in [
            ("", ""),
            ("", "0.0.0.0:8085"),
            ("127.0.0.1:8085", ""),
            (":", ":"),
            ("::::", "0.0.0.0:8085"),
            ("no-port", "also-no-port"),
            ("127.0.0.1:notaport", "0.0.0.0:8085"),
        ] {
            let _ = advertised_address_is_unreachable(advertised, bind);
        }
        assert!(!advertised_address_is_unreachable("127.0.0.1:8085", ""));
    }
}

#[cfg(test)]
mod shard_map_tests {
    use super::*;

    #[test]
    fn an_unconfigured_map_is_a_single_entry_that_never_routes() {
        let m = ShardMap::parse("lyracore", "");
        assert_eq!(m.resolve(0, 0), "lyracore");
        assert_eq!(m.resolve(389, 12345), "lyracore");
        assert_eq!(m.databases(), vec!["lyracore".to_string()]);
    }

    #[test]
    fn a_map_rule_routes_that_map_only() {
        let m = ShardMap::parse("world", "1:*=instances");
        assert_eq!(m.resolve(1, 0), "instances");
        assert_eq!(m.resolve(1, 99), "instances");
        assert_eq!(
            m.resolve(0, 0),
            "world",
            "unmatched maps stay on the default shard"
        );
        assert_eq!(
            m.databases(),
            vec!["world".to_string(), "instances".to_string()]
        );
    }

    #[test]
    fn a_bucket_rule_splits_one_maps_instances_across_a_pool() {
        let m = ShardMap::parse("world", "389:0=pool-a\n389:1=pool-b\n389:*=pool-c");
        assert_eq!(m.resolve(389, 0), "pool-a");
        assert_eq!(m.resolve(389, 8), "pool-a");
        assert_eq!(m.resolve(389, 1), "pool-b");
        assert_eq!(m.resolve(389, 2), "pool-c");
        assert_eq!(m.resolve(0, 0), "world");
    }

    #[test]
    fn first_matching_rule_wins_and_the_shorthand_omits_the_bucket() {
        let m = ShardMap::parse("world", "1=first; 1:*=second");
        assert_eq!(m.resolve(1, 3), "first");
    }

    fn flags<'a>(pairs: &'a [(&'a str, bool)]) -> impl Fn(&str) -> Option<bool> + 'a {
        move |db: &str| pairs.iter().find(|(d, _)| *d == db).map(|(_, f)| *f)
    }

    #[test]
    fn no_shard_map_plus_hosting_off_is_fatal_and_names_both_remedies() {
        let single = ShardMap::parse("lyracore", "");
        let verdict =
            single.check_instance_hosting(&[], flags(&[("lyracore", false)]), |d| d == "lyracore");
        let msg = match &verdict {
            InstanceHosting::NoHost(m) => m.clone(),
            other => panic!(
                "a single-database gateway against a database that does not host instance \
                 populations must be FATAL — nothing else can spawn the dungeon. Got: {other:?}"
            ),
        };
        assert!(
            msg.contains("UPDATE game_config SET hosts_instances = true WHERE id = 0"),
            "the message must name the exact SQL that fixes it: {msg}"
        );
        assert!(
            msg.contains("LYRACORE_SHARD_MAP"),
            "…or the routing that fixes it instead: {msg}"
        );
        assert!(
            msg.contains("lyracore"),
            "…and the database to run it on: {msg}"
        );
        assert!(
            verdict.enforce().is_err(),
            "the empty-dungeon verdict must refuse to start the gateway"
        );
    }

    #[test]
    fn the_unconfigured_single_database_realm_is_silent() {
        let single = ShardMap::parse("lyracore", "");
        let up = |d: &str| d == "lyracore";
        assert_eq!(
            single.check_instance_hosting(&[], flags(&[("lyracore", true)]), up),
            InstanceHosting::Consistent,
            "a single database that hosts its own dungeons is correctly configured — saying \
             ANYTHING here is noise the detector was built to remove"
        );
        assert_eq!(
            single.check_instance_hosting(&[], |_| None, up),
            InstanceHosting::Consistent,
            "a database with no game_config row reads as HOSTING (the module's own unwrap_or(true)) \
            — an unreadable config must never fabricate a fatal"
        );
    }

    #[test]
    fn a_routed_dungeon_warns_only_while_the_world_shard_still_hosts() {
        let phase_a = ShardMap::parse("lyracore", "36:*=lyracore-instances");
        let up = |_: &str| true;
        match phase_a.check_instance_hosting(
            &[],
            flags(&[("lyracore", true), ("lyracore-instances", true)]),
            up,
        ) {
            InstanceHosting::LoadNotMoved(msg) => {
                assert!(
                    msg.contains("UPDATE game_config SET hosts_instances = false WHERE id = 0"),
                    "the warning must name the exact SQL: {msg}"
                );
                assert!(msg.contains("lyracore"), "…on the world shard: {msg}");
            }
            other => panic!(
                "a world shard that still hosts populations for a map routed elsewhere spawns the \
                 dungeon then evicts it — that must warn. Got: {other:?}"
            ),
        }
        assert_eq!(
            phase_a.check_instance_hosting(
                &[],
                flags(&[("lyracore", false), ("lyracore-instances", true)]),
                up
            ),
            InstanceHosting::Consistent,
            "the CORRECT sharded configuration must be completely silent — an always-firing \
             warning gets filtered, which is exactly how the empty-dungeon failure went unnoticed"
        );
    }

    #[test]
    fn a_shard_map_that_routes_everything_except_the_dungeon_is_still_fatal() {
        let m = ShardMap::parse("lyracore", "1:*=lyracore-kalimdor");
        let verdict = m.check_instance_hosting(
            &[],
            flags(&[("lyracore", false), ("lyracore-kalimdor", true)]),
            |_| true,
        );
        assert!(
            matches!(verdict, InstanceHosting::NoHost(_)),
            "map 36 still resolves to the world shard, which does not host populations — the \
             presence of some OTHER hosting database does not populate Deadmines. Got: {verdict:?}"
        );
    }

    #[test]
    fn one_unhosted_pool_member_out_of_eight_is_still_fatal() {
        let m = ShardMap::parse("lyracore", "36:1=instances-1, 36:*=instances-0");
        let verdict = m.check_instance_hosting(
            &[],
            flags(&[
                ("lyracore", false),
                ("instances-0", true),
                ("instances-1", false),
            ]),
            |_| true,
        );
        assert!(
            matches!(verdict, InstanceHosting::NoHost(_)),
            "bucket 1 of map 36 lands on a database that hosts nothing — every 8th Deadmines run \
             would be an empty dungeon. Got: {verdict:?}"
        );
    }

    #[test]
    fn an_unreachable_instances_shard_is_reported_but_does_not_stop_the_gateway() {
        let phase_a = ShardMap::parse("lyracore", "36:*=lyracore-instances");
        let verdict = phase_a.check_instance_hosting(
            &[],
            flags(&[("lyracore", false), ("lyracore-instances", true)]),
            |d| d == "lyracore",
        );
        match &verdict {
            InstanceHosting::UnreachableHost(msg) => {
                assert!(
                    msg.contains("lyracore-instances") && msg.contains("NOT CONNECTED"),
                    "the message must name the shard that is down and why dungeons are empty: {msg}"
                );
                assert!(
                    msg.contains("restart the gateway"),
                    "…and the recovery, since the watchdog never re-runs this check: {msg}"
                );
            }
            other => panic!(
                "a correctly-routed realm whose instances shard is merely DOWN must come up and say \
                 so, not refuse to boot — the fatal is for a misconfiguration nothing can recover \
                 from. Got: {other:?}"
            ),
        }
        assert_eq!(
            verdict.enforce(),
            Ok(()),
            "refusing to start here converts a transient blip in one database into a whole-realm \
             outage: `main` exits, nothing retries, and the open world was never affected"
        );
    }

    #[test]
    fn the_operators_live_three_database_stack_is_silent() {
        let live = ShardMap::parse("lyracore", "36:*=lyracore-instances")
            .with_realm_core(Some("lyracore-realm"));
        let up = |d: &str| matches!(d, "lyracore" | "lyracore-instances" | "lyracore-realm");
        assert_eq!(
            live.check_instance_hosting(
                &[],
                flags(&[
                    ("lyracore", false),
                    ("lyracore-instances", true),
                    ("lyracore-realm", false),
                ]),
                up
            ),
            InstanceHosting::Consistent,
            "the deployed sharded Realm is correct and must produce NO output at all"
        );
        assert_eq!(
            live.check_instance_hosting(
                &[],
                flags(&[
                    ("lyracore", false),
                    ("lyracore-instances", true),
                    ("lyracore-realm", true),
                ]),
                up
            ),
            InstanceHosting::Consistent,
            "realm-core is not in `rules`, so it can never be a dungeon owner — its flag must not \
             be able to produce a verdict in either direction"
        );
    }

    #[test]
    fn only_the_unrecoverable_verdict_refuses_to_start() {
        assert!(InstanceHosting::Consistent.enforce().is_ok());
        assert!(
            InstanceHosting::LoadNotMoved("waste".into())
                .enforce()
                .is_ok(),
            "spawning a population that gets evicted is wasteful, not broken — it must not stop a \
             live realm from coming up"
        );
        assert!(
            InstanceHosting::UnreachableHost("down".into()).enforce().is_ok(),
            "another database being DOWN is transient and recoverable, and the open world is fine — \
             failing startup on it trades a degradation for an outage with no retry to escape it"
        );
        assert_eq!(
            InstanceHosting::NoHost("empty".into()).enforce(),
            Err("empty".into()),
            "an every-dungeon-is-empty realm whose owning database is CONNECTED and says it hosts \
             nothing cannot recover by waiting — that one must fail startup, with the message as the \
             reason"
        );
    }

    #[test]
    fn a_full_wildcard_rule_moves_the_whole_world() {
        let m = ShardMap::parse("world", "*:*=elsewhere");
        assert_eq!(m.resolve(0, 0), "elsewhere");
        assert_eq!(m.resolve(571, 7), "elsewhere");
    }

    #[test]
    fn an_unreachable_shard_degrades_to_the_default_database_not_to_the_asker() {
        let m = ShardMap::parse("world", "1:*=pool-a, 2:*=pool-b");
        let up = |db: &str| db != "pool-b";
        assert_eq!(
            m.resolve_connected(1, 0, up),
            "pool-a",
            "a reachable shard still routes"
        );
        assert_eq!(
            m.resolve_connected(2, 0, up),
            "world",
            "an unreachable shard falls back"
        );
        assert_eq!(
            m.resolve_connected(0, 0, up),
            "world",
            "unmatched maps are unaffected"
        );
    }

    #[test]
    fn the_existing_bucket_rule_spreads_consecutive_runs_across_the_pool() {
        let m = ShardMap::parse(
            "world",
            "36:1=instances-1, 36:3=instances-1, 36:5=instances-1, 36:7=instances-1, \
             36:*=instances-0",
        );
        let landed: Vec<&str> = (1..=8).map(|id| m.resolve(36, id)).collect();
        assert_eq!(
            landed,
            [
                "instances-1",
                "instances-0",
                "instances-1",
                "instances-0",
                "instances-1",
                "instances-0",
                "instances-1",
                "instances-0",
            ],
            "consecutive runs must alternate across the pool — that IS the load spread"
        );
        assert_eq!(m.instance_pool(36), vec!["instances-0", "instances-1"]);
        assert_eq!(m.resolve(0, 0), "world");
        assert_eq!(
            m.instance_pool(0),
            vec!["world"],
            "map 0's 'pool' is the world shard itself"
        );
    }

    #[test]
    fn a_wildcard_bucket_rule_puts_the_open_world_in_the_pool_and_says_so() {
        let trap = ShardMap::parse("world", "*:0=pool-a");
        assert_eq!(
            trap.resolve(0, 0),
            "pool-a",
            "the whole open world moved — the documented trap"
        );
        assert_eq!(trap.instance_pool(0), vec!["pool-a", "world"]);
        let named = ShardMap::parse("world", "36:0=pool-a");
        assert_eq!(named.resolve(0, 0), "world");
        assert_eq!(named.instance_pool(36), vec!["pool-a", "world"]);
    }

    #[test]
    fn an_unconfigured_map_has_a_one_database_pool_and_instance_owner_is_resolve() {
        let m = ShardMap::parse("lyracore", "");
        assert_eq!(m.instance_pool(0), vec!["lyracore"]);
        assert_eq!(m.instance_pool(36), vec!["lyracore"]);
        for map_id in [0u32, 1, 36, 389] {
            for instance_id in [0u64, 1, 5, 8, 12345] {
                assert_eq!(
                    m.instance_owner(map_id, instance_id, "lyracore", |_| true),
                    m.resolve(map_id, instance_id),
                    "map {map_id}/instance {instance_id} must route identically with and without \
                     the stickiness rule when nothing is configured"
                );
            }
        }
    }

    #[test]
    fn adding_a_second_instances_database_moves_new_runs_and_leaves_a_live_run_where_it_is() {
        let up = |_: &str| true;
        let before = ShardMap::parse("world", "36:*=instances-0");
        assert_eq!(before.instance_pool(36), vec!["instances-0"]);
        assert_eq!(before.instance_owner(36, 5, "world", up), "instances-0");

        let after = ShardMap::parse(
            "world",
            "36:4=instances-1, 36:5=instances-1, 36:6=instances-1, 36:7=instances-1, \
             36:*=instances-0",
        );
        assert_eq!(after.instance_pool(36), vec!["instances-0", "instances-1"]);
        assert_eq!(
            after.instance_owner(36, 13, "world", up),
            "instances-1",
            "13 % 8 == 5"
        );
        assert_eq!(
            after.instance_owner(36, 10, "world", up),
            "instances-0",
            "10 % 8 == 2"
        );
        assert_eq!(
            after.instance_owner(36, 5, "instances-0", up),
            "instances-0",
            "a run already live on a pool member must survive that bucket being re-pointed"
        );
        assert_eq!(after.resolve(36, 5), "instances-1");
    }

    #[test]
    fn stickiness_never_applies_to_the_open_world_or_to_a_shard_outside_the_pool() {
        let up = |_: &str| true;
        let m = ShardMap::parse("world", "36:*=instances-0");
        assert_eq!(m.instance_owner(0, 0, "instances-0", up), "world");
        let split = ShardMap::parse("world", "0:0=world-a, 0:*=world-b");
        assert_eq!(split.instance_pool(0), vec!["world-a", "world-b"]);
        assert_eq!(split.resolve(0, 0), "world-a");
        assert_eq!(
            split.instance_owner(0, 0, "world-b", up),
            "world-a",
            "the open world is never sticky — stickiness is a property of INSTANCES"
        );
        assert_eq!(m.instance_owner(36, 5, "world", up), "instances-0");
        let two = ShardMap::parse("world", "36:*=instances-0, 389:*=instances-1");
        assert_eq!(two.instance_owner(36, 5, "instances-1", up), "instances-0");
    }

    #[test]
    fn a_sticky_holder_that_is_not_connected_falls_back_to_the_shard_map() {
        let m = ShardMap::parse("world", "36:*=instances-0");
        assert_eq!(
            m.instance_owner(36, 5, "instances-0", |db| db != "instances-0"),
            "world"
        );
        assert_eq!(
            m.instance_owner(36, 5, "instances-0", |_| true),
            "instances-0"
        );
    }

    #[test]
    fn realm_core_unconfigured_keeps_auth_on_the_default_database() {
        let m = ShardMap::parse("world", "1:*=instances");
        assert_eq!(m.realm_core_db(), None);
        assert_eq!(m.auth_db(|_| true), Ok("world"));
        assert_eq!(
            m.auth_db(|_| false),
            Ok("world"),
            "an unconfigured realm-core cannot fail closed"
        );
        assert_eq!(m.databases(), m.shards(), "nothing extra to connect to");
    }

    #[test]
    fn a_configured_realm_core_owns_auth_and_gets_its_own_connection() {
        let m = ShardMap::parse("world", "1:*=instances").with_realm_core(Some("lyracore-realm"));
        assert_eq!(m.realm_core_db(), Some("lyracore-realm"));
        assert_eq!(m.auth_db(|_| true), Ok("lyracore-realm"));
        assert_eq!(
            m.databases(),
            vec![
                "world".to_string(),
                "instances".to_string(),
                "lyracore-realm".to_string()
            ],
            "realm-core is connected but is NOT a shard"
        );
        assert_eq!(
            m.shards(),
            vec!["world".to_string(), "instances".to_string()],
            "realm-core holds no characters, so a character probe must never walk it"
        );
    }

    #[test]
    fn an_unreachable_realm_core_fails_closed_instead_of_falling_back_to_the_world_cache() {
        let m = ShardMap::parse("world", "").with_realm_core(Some("lyracore-realm"));
        assert_eq!(
            m.auth_db(|db| db != "lyracore-realm"),
            Err("lyracore-realm")
        );
    }

    #[test]
    fn realm_core_naming_the_world_database_or_nothing_is_treated_as_unconfigured() {
        for cfg in [Some("world"), Some("  "), Some(""), None] {
            let m = ShardMap::parse("world", "").with_realm_core(cfg);
            assert_eq!(
                m.realm_core_db(),
                None,
                "LYRACORE_REALM_CORE={cfg:?} must read as unconfigured"
            );
            assert_eq!(m.databases(), vec!["world".to_string()]);
        }
        let m = ShardMap::parse("world", "").with_realm_core(Some(" lyracore-realm "));
        assert_eq!(m.realm_core_db(), Some("lyracore-realm"));
    }

    #[test]
    fn the_unconfigured_realm_is_exactly_one_database_in_every_accessor() {
        let m = ShardMap::parse("lyracore", "").with_realm_core(None);

        assert_eq!(m.default_db(), "lyracore");
        assert_eq!(m.shards(), vec!["lyracore".to_string()]);
        assert_eq!(m.databases(), vec!["lyracore".to_string()]);
        assert_eq!(
            m.auth_db(|_| true),
            Ok("lyracore"),
            "unsharded, auth is served from the one database — there is no realm-core to consult"
        );
        assert_eq!(
            m.auth_db(|_| false),
            Ok("lyracore"),
            "and with no realm-core configured, connectivity of OTHER databases cannot make auth \
             fail closed"
        );

        for map_id in [
            0u32,
            1,
            30,
            36,
            43,
            47,
            70,
            90,
            109,
            129,
            189,
            209,
            389,
            4_000_000_000,
        ] {
            for instance_id in [0u64, 1, 7, 63, 64, 65, 1_000_000, u64::MAX] {
                assert_eq!(
                    m.resolve(map_id, instance_id),
                    "lyracore",
                    "map {map_id}/instance {instance_id} routed off the one database"
                );
            }
            assert_eq!(
                m.instance_pool(map_id),
                vec!["lyracore"],
                "map {map_id}'s instance pool must be the one database"
            );
        }

        assert_eq!(
            m.check_instance_hosting(&[], |_| Some(true), |_| true),
            InstanceHosting::Consistent,
            "a single-database realm hosting its own instances has nothing to report"
        );
    }

    #[test]
    fn every_separator_spelling_of_the_same_map_parses_identically() {
        let rules = ["1:*=world-1", "36:0=inst-a", "36:*=inst-b", "*:*=world-2"];
        let spellings = [
            rules.join(","),
            rules.join(";"),
            rules.join("\n"),
            format!(
                "  {}  ;\n\n  {} ,\n{}\n\n;  {}  ",
                rules[0], rules[1], rules[2], rules[3]
            ),
            format!(
                "# the realm\n{} # eastern kingdoms\n{},{}\n{}",
                rules[0], rules[1], rules[2], rules[3]
            ),
        ];

        let canonical = ShardMap::parse("core", &spellings[0]);
        assert_eq!(
            canonical.shards(),
            vec![
                "core".to_string(),
                "world-1".to_string(),
                "inst-a".to_string(),
                "inst-b".to_string(),
                "world-2".to_string()
            ],
            "precondition: the comma spelling parses all four rules — otherwise this test compares \
             two equally-broken maps and proves nothing"
        );

        for (i, text) in spellings.iter().enumerate().skip(1) {
            let m = ShardMap::parse("core", text);
            assert_eq!(
                m.shards(),
                canonical.shards(),
                "spelling {i} ({text:?}) parsed to a different set of databases"
            );
            for map_id in [0u32, 1, 36, 189] {
                for instance_id in [0u64, 1, 2, 64, 65] {
                    assert_eq!(
                        m.resolve(map_id, instance_id),
                        canonical.resolve(map_id, instance_id),
                        "spelling {i} routes map {map_id}/instance {instance_id} differently"
                    );
                }
            }
        }
    }

    #[test]
    fn each_malformed_rule_shape_is_dropped_whole_and_never_half_applied() {
        let garbage = [
            ("no separator at all", "nonsense"),
            ("empty database", "1:0="),
            ("whitespace-only database", "1:0=   "),
            ("no location", "=db"),
            ("non-numeric map id", "x:1=db"),
            ("non-numeric bucket", "1:x=db"),
            ("negative map id", "-1:0=db"),
            ("bucket outside the modulus", "1:99=db"),
            ("map id past u32", "4294967296:0=db"),
            ("empty rule", ""),
            ("comment only", "# just a note"),
        ];
        for (what, rule) in garbage {
            let m = ShardMap::parse("core", rule);
            assert_eq!(
                m.shards(),
                vec!["core".to_string()],
                "the {what} rule {rule:?} was accepted and added a database — a rule the parser \
                 cannot fully read must be dropped, not partly applied"
            );
            for map_id in [0u32, 1, 36] {
                for instance_id in [0u64, 1, 99] {
                    assert_eq!(
                        m.resolve(map_id, instance_id),
                        "core",
                        "the {what} rule {rule:?} rerouted map {map_id}/instance {instance_id}"
                    );
                }
            }
        }
    }

    #[test]
    fn any_operator_shard_map_text_degrades_and_never_routes_off_its_own_database_list() {
        use crate::codec::property_tests::Rng;
        let mut rng = Rng::new(0x5348_4152_4400);
        let pieces = [
            "1",
            "36",
            "*",
            "x",
            "-1",
            "0",
            "99",
            "4294967296",
            ":",
            "=",
            ",",
            ";",
            "\n",
            "#",
            "world-1",
            "core",
            " ",
            "",
            "::",
            "==",
            "1:",
            ":0",
            "=db",
        ];

        for _ in 0..3_000 {
            let mut text = String::new();
            for _ in 0..rng.below(12) {
                text.push_str(pieces[rng.below(pieces.len())]);
            }
            let m = ShardMap::parse("core", &text);
            let known = m.databases();
            assert!(
                known.contains(&"core".to_string()),
                "the default database must always be in the list; text was {text:?}"
            );
            for map_id in [0u32, 1, 36, 4_000_000_000] {
                for instance_id in [0u64, 1, 63, u64::MAX] {
                    let db = m.resolve(map_id, instance_id).to_string();
                    assert!(
                        known.contains(&db),
                        "shard map {text:?} routed map {map_id}/instance {instance_id} to {db:?}, \
                         which is not in its own database list {known:?} — the gateway never \
                         connects to it, so this is a mid-session hang rather than a startup error"
                    );
                }
            }
        }
    }

    #[test]
    fn malformed_rules_are_dropped_and_the_map_degrades_to_the_default() {
        let m = ShardMap::parse(
            "world",
            "# a comment\n\n nonsense , 1:=  , =db , x:1=db , 1:99=db , 1:*=good",
        );
        assert_eq!(m.resolve(0, 0), "world");
        assert_eq!(m.resolve(1, 0), "good", "the one VALID rule still applies");
        assert_eq!(m.databases(), vec!["world".to_string(), "good".to_string()]);
    }
}

#[cfg(test)]
mod admission_limit_tests {
    use super::parse_admission_limit;
    use std::ffi::OsStr;

    #[test]
    fn absent_or_zero_limits_allow_unlimited_admission() {
        assert_eq!(parse_admission_limit("LIMIT", None).unwrap(), 0);
        assert_eq!(
            parse_admission_limit("LIMIT", Some(OsStr::new("0"))).unwrap(),
            0
        );
    }

    #[test]
    fn decimal_limits_preserve_the_configured_value() {
        for (raw, expected) in [("1", 1), ("1000", 1000), ("  2048 ", 2048)] {
            assert_eq!(
                parse_admission_limit("LIMIT", Some(OsStr::new(raw))).unwrap(),
                expected
            );
        }
        assert_eq!(
            parse_admission_limit("LIMIT", Some(OsStr::new(&usize::MAX.to_string()))).unwrap(),
            usize::MAX
        );
    }

    #[test]
    fn malformed_limits_do_not_become_unlimited() {
        let overflow = (usize::MAX as u128 + 1).to_string();
        for name in ["LYRACORE_MAX_SESSIONS", "LYRACORE_ADMIT_CONCURRENCY"] {
            for raw in [
                "", "  ", "-1", "+1", "1,000", "1_000", "1e3", "1.5", "many", &overflow,
            ] {
                let error = parse_admission_limit(name, Some(OsStr::new(raw)))
                    .unwrap_err()
                    .to_string();
                assert!(error.contains(name), "{error}");
                assert!(error.contains(&format!("{raw:?}")), "{error}");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_limit_is_an_error() {
        use std::os::unix::ffi::OsStrExt;
        assert!(parse_admission_limit("LIMIT", Some(OsStr::from_bytes(&[0xff]))).is_err());
    }
    #[test]
    fn imported_raid_maps_require_hosts_through_default_and_wildcard_routes() {
        for rules in ["36:*=instances", "36:*=instances, *:*=world"] {
            let map = crate::config::ShardMap::parse("world", rules);
            let verdict =
                map.check_instance_hosting(&[533], |shard| Some(shard == "instances"), |_| true);
            assert!(
                matches!(verdict, crate::config::InstanceHosting::NoHost(ref reason) if reason.contains("533")),
                "{rules}: {verdict:?}"
            );
            assert!(matches!(
                map.check_instance_hosting(&[], |shard| Some(shard == "instances"), |_| true),
                crate::config::InstanceHosting::Consistent
            ));
        }
    }

    #[test]
    fn configured_raid_routes_require_population_hosts() {
        let map = crate::config::ShardMap::parse("lyracore", "36:*=instances, 533:*=raids");
        assert!(matches!(
            map.check_instance_hosting(&[], |db| Some(db != "raids"), |_| true),
            crate::config::InstanceHosting::NoHost(_)
        ));
    }
}
