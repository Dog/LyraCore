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

pub mod limiter;

use crate::accept::{classify_accept_error, AcceptBackoff, AcceptOutcome};
use crate::read_deadline::{
    is_io_deadline, DeadlineClock, IoDeadline, PreAuthDeadline, LOGON_AUTH_DEADLINE,
};
use crate::{config::GatewayConfig, stdb::Coordinator};
use anyhow::{anyhow, Result};
use limiter::{LogonConnection, LogonLimiter, PROOF_FAILURE_DELAY};
use std::io::{Read, Write};
use std::time::Instant;
use tokio::net::TcpListener;
use wow_login_messages::all::CMD_AUTH_LOGON_CHALLENGE_Client;
use wow_login_messages::errors::ExpectedOpcodeError;
use wow_login_messages::version_3::opcodes::ClientOpcodeMessage;
use wow_login_messages::version_3::{
    CMD_AUTH_LOGON_CHALLENGE_Server, CMD_AUTH_LOGON_CHALLENGE_Server_SecurityFlag,
    CMD_AUTH_LOGON_PROOF_Server, CMD_REALM_LIST_Server, Realm, RealmCategory, RealmFlag, RealmType,
};
use wow_login_messages::Message;
use wow_srp::normalized_string::NormalizedString;
use wow_srp::server::{SrpProof, SrpVerifier};
use wow_srp::{PublicKey, GENERATOR, LARGE_SAFE_PRIME_LITTLE_ENDIAN};

#[derive(Clone, Debug)]
pub struct LogonAccount {
    pub id: u64,
    pub salt: [u8; 32],
    pub verifier: [u8; 32],
    pub banned: bool,
}

#[derive(Clone, Debug)]
pub struct RealmInfo {
    pub id: u8,
    pub name: String,
    pub address: String,
    pub realm_type: u32,
    pub population: f32,
    pub number_of_characters: u8,
}

pub trait LogonStore: Send + Sync {
    fn account(&self, username: &str) -> Result<Option<LogonAccount>>;
    fn bound_identity(&self, account_id: u64, username: &str) -> Result<[u8; 32]>;
    fn save_session(
        &self,
        account_id: u64,
        username: &str,
        session_key: &[u8; 40],
        bound_identity: [u8; 32],
    ) -> Result<()>;
    fn realms(&self, account_id: u64, username: &str) -> Result<Vec<RealmInfo>>;
}

#[cfg(test)]
pub fn handle_logon<S: Read + Write + IoDeadline, St: LogonStore + ?Sized>(
    stream: &mut S,
    store: &St,
) -> Result<()> {
    let mut deadline = PreAuthDeadline::after(LOGON_AUTH_DEADLINE);
    handle_logon_with_deadline(stream, store, &mut deadline, &mut loopback_connection())
}

#[cfg(test)]
fn loopback_connection() -> LogonConnection {
    LogonLimiter::new()
        .admit(std::net::Ipv4Addr::LOCALHOST.into(), Instant::now())
        .expect("a fresh limiter admits its first connection")
}

fn handle_logon_with_deadline<
    S: Read + Write + IoDeadline,
    St: LogonStore + ?Sized,
    C: DeadlineClock,
>(
    stream: &mut S,
    store: &St,
    deadline: &mut PreAuthDeadline<C>,
    connection: &mut LogonConnection,
) -> Result<()> {
    let mut pending: Option<(SrpProof, u64, String)> = None;
    let mut authenticated: Option<(u64, String)> = None;

    loop {
        let read = if authenticated.is_some() {
            ClientOpcodeMessage::read(&mut *stream)
        } else {
            ClientOpcodeMessage::read(&mut deadline.io(stream))
        };
        let msg = match read {
            Ok(m) => m,
            Err(ExpectedOpcodeError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Ok(());
            }
            Err(ExpectedOpcodeError::Io(e)) if is_io_deadline(&e) => {
                return Err(anyhow!(
                    "no logon packet before the total pre-auth deadline"
                ));
            }
            Err(ExpectedOpcodeError::Io(e)) => {
                return Err(anyhow::Error::new(e).context("logon read failed"));
            }
            Err(e) => return Err(anyhow!("logon read error: {e}")),
        };

        match msg {
            ClientOpcodeMessage::CMD_AUTH_LOGON_CHALLENGE(c) => {
                connection.start_attempt(Instant::now())?;
                pending = handle_challenge(&mut deadline.io(stream), store, &c, connection)?;
            }
            ClientOpcodeMessage::CMD_AUTH_LOGON_PROOF(p) => {
                let Some((proof, account_id, username)) = pending.take() else {
                    return Err(anyhow!("CMD_AUTH_LOGON_PROOF before a challenge"));
                };
                authenticated = handle_proof(
                    &mut deadline.io(stream),
                    store,
                    proof,
                    account_id,
                    &username,
                    &p,
                    connection,
                )?
                .map(|id| (id, username));
                if authenticated.is_some() {
                    deadline.finish(stream)?;
                }
            }
            ClientOpcodeMessage::CMD_REALM_LIST(_) => {
                let realms = match &authenticated {
                    Some((id, username)) => store.realms(*id, username)?,
                    None => Vec::new(),
                };
                let reply = CMD_REALM_LIST_Server {
                    realms: realms.iter().map(to_realm).collect(),
                };
                let mut bytes = Vec::new();
                reply.write(&mut bytes)?;
                log::info!(
                    "logon: realm list ({} realm(s), {} bytes) to account {:?}: {:02x?}",
                    realms.len(),
                    bytes.len(),
                    authenticated.as_ref().map(|(id, _)| id),
                    bytes
                );
                if authenticated.is_some() {
                    stream.write_all(&bytes)?;
                } else {
                    deadline.io(stream).write_all(&bytes)?;
                }
            }
            other => log::debug!("logon: ignoring opcode {other}"),
        }
    }
}

fn handle_challenge<S: Read + Write, St: LogonStore + ?Sized>(
    stream: &mut S,
    store: &St,
    c: &CMD_AUTH_LOGON_CHALLENGE_Client,
    connection: &mut LogonConnection,
) -> Result<Option<(SrpProof, u64, String)>> {
    let mut refuse = |reply: CMD_AUTH_LOGON_CHALLENGE_Server| -> Result<_> {
        let attempt = connection.record_failure(Instant::now());
        reply.write(&mut *stream)?;
        attempt?;
        Ok(None)
    };
    let username = c.account_name.to_uppercase();
    let account = match store.account(&username)? {
        Some(a) if a.banned => {
            return refuse(CMD_AUTH_LOGON_CHALLENGE_Server::FailBanned);
        }
        Some(a) => a,
        None => {
            return refuse(CMD_AUTH_LOGON_CHALLENGE_Server::FailUnknownAccount);
        }
    };

    log::info!(
        "logon: challenge for {username} (account {}), client protocol_version={:?}",
        account.id,
        c.protocol_version
    );
    // wow_srp panics on the all-zero verifier used by unprovisioned accounts.
    if account.verifier.iter().all(|&b| b == 0) {
        log::warn!(
            "logon: account {} ({username}) has no SRP6 verifier (not provisioned) — rejecting",
            account.id
        );
        return refuse(CMD_AUTH_LOGON_CHALLENGE_Server::FailUnknownAccount);
    }
    let ns = NormalizedString::new(&username).map_err(|e| anyhow!("bad username: {e}"))?;
    let proof = SrpVerifier::from_database_values(ns, account.verifier, account.salt).into_proof();

    CMD_AUTH_LOGON_CHALLENGE_Server::Success {
        crc_salt: [0u8; 16],
        generator: vec![GENERATOR],
        large_safe_prime: LARGE_SAFE_PRIME_LITTLE_ENDIAN.to_vec(),
        salt: *proof.salt(),
        server_public_key: *proof.server_public_key(),
        // Vanilla clients require this trailing byte; None means no PIN or matrix challenge.
        security_flag: CMD_AUTH_LOGON_CHALLENGE_Server_SecurityFlag::None,
    }
    .write(&mut *stream)?;

    Ok(Some((proof, account.id, username)))
}

fn handle_proof<S: Read + Write, St: LogonStore + ?Sized>(
    stream: &mut S,
    store: &St,
    proof: SrpProof,
    account_id: u64,
    username: &str,
    packet: &wow_login_messages::version_3::CMD_AUTH_LOGON_PROOF_Client,
    connection: &mut LogonConnection,
) -> Result<Option<u64>> {
    let verified = PublicKey::from_le_bytes(packet.client_public_key)
        .ok()
        .and_then(|key| proof.into_server(key, packet.client_proof).ok());
    let Some((server, server_proof)) = verified else {
        let attempt = connection.record_failure(Instant::now());
        std::thread::sleep(PROOF_FAILURE_DELAY);
        CMD_AUTH_LOGON_PROOF_Server::FailIncorrectPassword.write(&mut *stream)?;
        attempt?;
        return Ok(None);
    };

    let session_key = *server.session_key();
    let bound = store.bound_identity(account_id, username)?;
    store.save_session(account_id, username, &session_key, bound)?;
    log::info!("logon: account {account_id} authenticated (SRP6 proof OK, K + session persisted)");

    CMD_AUTH_LOGON_PROOF_Server::Success {
        hardware_survey_id: 0,
        server_proof,
    }
    .write(&mut *stream)?;
    Ok(Some(account_id))
}

fn to_realm(r: &RealmInfo) -> Realm {
    Realm {
        realm_type: match r.realm_type {
            1 => RealmType::PlayerVsPlayer,
            6 => RealmType::Roleplaying,
            8 => RealmType::RoleplayingPlayerVsPlayer,
            _ => RealmType::PlayerVsEnvironment,
        },
        flag: RealmFlag::empty(),
        name: r.name.clone(),
        address: r.address.clone(),
        population: r.population.into(),
        number_of_characters_on_realm: r.number_of_characters,
        category: RealmCategory::Default,
        realm_id: r.id,
    }
}

pub async fn run(cfg: GatewayConfig, coordinator: Coordinator) -> Result<()> {
    let listener = TcpListener::bind(&cfg.logon_bind).await?;
    log::info!("logon listening on {}", cfg.logon_bind);
    let mut backoff = AcceptBackoff::new();
    let mut capacity_was_full = false;
    let limiter = LogonLimiter::new();
    loop {
        let (sock, peer) = match listener.accept().await {
            Ok(pair) => {
                backoff.record_success();
                pair
            }
            Err(e) => match classify_accept_error(&e) {
                AcceptOutcome::Fatal => {
                    log::error!(
                        "logon listener is unusable and cannot accept again: {e} — ending the \
                         logon task"
                    );
                    return Err(e.into());
                }
                AcceptOutcome::Retry => {
                    let delay = backoff.record_failure();
                    log::warn!(
                        "logon accept failed ({e}); skipping this connection (consecutive={}, \
                         backing off {}ms)",
                        backoff.consecutive(),
                        delay.as_millis()
                    );
                    if !delay.is_zero() {
                        tokio::time::sleep(delay).await;
                    }
                    continue;
                }
            },
        };
        let mut connection = match limiter.admit(peer.ip(), Instant::now()) {
            Ok(connection) => connection,
            Err(refusal) => {
                refusal.count();
                log::debug!("logon: refused {peer}: {refusal}");
                continue;
            }
        };
        let mut deadline = PreAuthDeadline::after(LOGON_AUTH_DEADLINE);
        let Some(task_permit) = cfg.blocking_task_capacity.try_admit() else {
            if !capacity_was_full {
                log::warn!(
                    "logon blocking-task capacity is full; rejecting new connections before they \
                     enter the blocking-task queue"
                );
                capacity_was_full = true;
            }
            continue;
        };
        capacity_was_full = false;
        let coord = coordinator.clone();
        let std_sock = match sock.into_std().and_then(|s| {
            s.set_nonblocking(false)?;
            Ok(s)
        }) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("logon connection {peer} could not be handed to a blocking task: {e}");
                continue;
            }
        };
        if let Err(e) = deadline.arm(&std_sock) {
            log::warn!("logon connection {peer} could not arm its pre-auth watchdog: {e}");
            continue;
        }
        tokio::task::spawn_blocking(move || {
            let _task_permit = task_permit;
            let store = CoordinatorStore::new(coord);
            let mut s = std_sock;
            if let Err(e) =
                handle_logon_with_deadline(&mut s, &store, &mut deadline, &mut connection)
            {
                let disconnected = e.downcast_ref::<std::io::Error>().is_some_and(|error| {
                    matches!(
                        error.kind(),
                        std::io::ErrorKind::BrokenPipe
                            | std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::ConnectionAborted
                    )
                });
                if let Some(refusal) = e.downcast_ref::<limiter::LogonRefusal>() {
                    refusal.count();
                }
                if e.is::<limiter::LogonRefusal>() || disconnected {
                    log::debug!("logon connection {peer} ended: {e:#}");
                } else {
                    log::warn!("logon connection {peer} ended: {e:#}");
                }
            }
        });
    }
}

pub(crate) struct CoordinatorStore<D: crate::realm_core::RealmDb> {
    coordinator: D,
}

impl<D: crate::realm_core::RealmDb> CoordinatorStore<D> {
    pub(crate) fn new(coordinator: D) -> Self {
        Self { coordinator }
    }

    fn world_account_id(&self, realm_account_id: u64, username: &str) -> Result<u64> {
        world_account_id(
            realm_account_id,
            username,
            self.coordinator.realm_core()?.shard_name() == self.coordinator.shard_name(),
            self.coordinator.shard_name(),
            || {
                Ok(self
                    .coordinator
                    .account_by_username(username)?
                    .map(|a| a.id))
            },
        )
    }
}

// Account ids are shard-local; only the username is realm-wide.
fn world_account_id(
    realm_account_id: u64,
    username: &str,
    same_database: bool,
    world_db: &str,
    lookup: impl FnOnce() -> Result<Option<u64>>,
) -> Result<u64> {
    if same_database {
        return Ok(realm_account_id);
    }
    lookup()?.ok_or_else(|| {
        anyhow!(
            "account {username} exists on realm-core but not on world shard {world_db} — provision \
             it there before it can log in (accounts are keyed across databases by USERNAME; ids \
             are per-database and must never be carried between them)"
        )
    })
}

impl<D: crate::realm_core::RealmDb> LogonStore for CoordinatorStore<D> {
    fn account(&self, username: &str) -> Result<Option<LogonAccount>> {
        let Some(a) = self
            .coordinator
            .realm_core()?
            .account_by_username(username)?
        else {
            return Ok(None);
        };
        let salt = a.salt.try_into().map_err(|v: Vec<u8>| {
            anyhow!("account {}: salt is {} bytes, expected 32", a.id, v.len())
        })?;
        let verifier = a.verifier.try_into().map_err(|v: Vec<u8>| {
            anyhow!(
                "account {}: verifier is {} bytes, expected 32",
                a.id,
                v.len()
            )
        })?;
        Ok(Some(LogonAccount {
            id: a.id,
            salt,
            verifier,
            banned: a.banned,
        }))
    }

    fn bound_identity(&self, account_id: u64, username: &str) -> Result<[u8; 32]> {
        let world_id = self.world_account_id(account_id, username)?;
        self.coordinator.bound_identity(world_id)
    }

    // Persist Realm-core's authoritative key before the World Shard's identity binding.
    fn save_session(
        &self,
        account_id: u64,
        username: &str,
        session_key: &[u8; 40],
        bound_identity: [u8; 32],
    ) -> Result<()> {
        let realm_core = self.coordinator.realm_core()?;
        realm_core.establish_session(account_id, session_key, bound_identity)?;
        if realm_core.shard_name() == self.coordinator.shard_name() {
            return Ok(());
        }
        self.coordinator.establish_session(
            self.world_account_id(account_id, username)?,
            session_key,
            bound_identity,
        )
    }

    fn realms(&self, account_id: u64, username: &str) -> Result<Vec<RealmInfo>> {
        let realm = self.coordinator.realm()?;
        let n = self
            .coordinator
            .character_count(self.world_account_id(account_id, username)?)?;
        Ok(vec![RealmInfo {
            id: realm.id,
            name: realm.name,
            address: crate::config::advertised_realm_address_or(realm.address),
            realm_type: realm.realm_type,
            population: realm.population,
            number_of_characters: n,
        }])
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::read_deadline::{DeadlineClock, PreAuthDeadline};
    use std::cell::Cell;
    use std::io::Cursor;
    use std::os::unix::net::UnixStream;
    use std::rc::Rc;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    #[derive(Clone)]
    struct ManualClock(Rc<Cell<Instant>>);

    impl DeadlineClock for ManualClock {
        fn now(&self) -> Instant {
            self.0.get()
        }
    }

    struct AdvancingStream {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
        clock: ManualClock,
        per_read: Duration,
        per_write: Duration,
        write_timeout_calls: Rc<Cell<usize>>,
    }

    impl Read for AdvancingStream {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let read = self.input.read(buf)?;
            self.clock.0.set(self.clock.0.get() + self.per_read);
            Ok(read)
        }
    }

    impl Write for AdvancingStream {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.output.extend_from_slice(buf);
            self.clock.0.set(self.clock.0.get() + self.per_write);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl IoDeadline for AdvancingStream {
        fn set_read_timeout(&self, _timeout: Option<Duration>) -> std::io::Result<()> {
            Ok(())
        }

        fn set_write_timeout(&self, _timeout: Option<Duration>) -> std::io::Result<()> {
            self.write_timeout_calls
                .set(self.write_timeout_calls.get() + 1);
            Ok(())
        }
    }
    use wow_login_messages::all::{Locale, Os, Platform, ProtocolVersion, Version};
    use wow_login_messages::version_3::opcodes::ServerOpcodeMessage;
    use wow_srp::client::SrpClientChallenge;

    struct InMemoryStore {
        account: LogonAccount,
        username: String,
        saved: Mutex<Option<[u8; 40]>>,
        usernames: Mutex<Vec<(&'static str, String)>>,
    }

    impl InMemoryStore {
        fn told(&self, method: &str) -> Option<String> {
            let seen = self.usernames.lock().unwrap();
            seen.iter()
                .find(|(m, _)| *m == method)
                .map(|(_, u)| u.clone())
        }
    }

    impl LogonStore for InMemoryStore {
        fn account(&self, username: &str) -> Result<Option<LogonAccount>> {
            Ok((username == self.username).then(|| self.account.clone()))
        }
        fn bound_identity(&self, _account_id: u64, username: &str) -> Result<[u8; 32]> {
            self.usernames
                .lock()
                .unwrap()
                .push(("bound_identity", username.to_string()));
            Ok([7u8; 32])
        }
        fn save_session(
            &self,
            _id: u64,
            username: &str,
            key: &[u8; 40],
            _b: [u8; 32],
        ) -> Result<()> {
            *self.saved.lock().unwrap() = Some(*key);
            self.usernames
                .lock()
                .unwrap()
                .push(("save_session", username.to_string()));
            Ok(())
        }
        fn realms(&self, _account_id: u64, username: &str) -> Result<Vec<RealmInfo>> {
            self.usernames
                .lock()
                .unwrap()
                .push(("realms", username.to_string()));
            Ok(vec![RealmInfo {
                id: 1,
                name: "LyraCore".into(),
                address: "127.0.0.1:8085".into(),
                realm_type: 0,
                population: 0.0,
                number_of_characters: 1,
            }])
        }
    }

    fn ns(s: &str) -> NormalizedString {
        NormalizedString::new(s).unwrap()
    }

    fn challenge_client(account: &str) -> CMD_AUTH_LOGON_CHALLENGE_Client {
        CMD_AUTH_LOGON_CHALLENGE_Client {
            protocol_version: ProtocolVersion::Three,
            version: Version {
                major: 1,
                minor: 12,
                patch: 1,
                build: 5875,
            },
            platform: Platform::X86,
            os: Os::Windows,
            locale: Locale::EnUs,
            utc_timezone_offset: 0,
            client_ip_address: std::net::Ipv4Addr::new(127, 0, 0, 1),
            account_name: account.to_string(),
        }
    }

    #[test]
    fn full_srp6_handshake_and_realm_list() {
        let v = SrpVerifier::from_username_and_password(ns("TEST"), ns("PASSWORD"));
        let salt = *v.salt();
        let verifier = *v.password_verifier();
        let store = std::sync::Arc::new(InMemoryStore {
            account: LogonAccount {
                id: 1,
                salt,
                verifier,
                banned: false,
            },
            username: "TEST".into(),
            saved: Mutex::new(None),
            usernames: Mutex::new(Vec::new()),
        });

        let (mut client, server_end) = UnixStream::pair().unwrap();
        let server_store = store.clone();
        let server = std::thread::spawn(move || {
            let mut s = server_end;
            handle_logon(&mut s, server_store.as_ref()).unwrap();
        });

        challenge_client("TEST").write(&mut client).unwrap();

        let (g, n, s_salt, server_pubkey) = match ServerOpcodeMessage::read(&mut client).unwrap() {
            ServerOpcodeMessage::CMD_AUTH_LOGON_CHALLENGE(
                CMD_AUTH_LOGON_CHALLENGE_Server::Success {
                    generator,
                    large_safe_prime,
                    salt,
                    server_public_key,
                    ..
                },
            ) => (generator, large_safe_prime, salt, server_public_key),
            other => panic!("expected challenge success, got {other:?}"),
        };
        assert_eq!(g, vec![7]);

        let n: [u8; 32] = n.try_into().unwrap();
        let challenge = SrpClientChallenge::new(
            ns("TEST"),
            ns("PASSWORD"),
            g[0],
            n,
            PublicKey::from_le_bytes(server_pubkey).unwrap(),
            s_salt,
        );
        let client_public_key = *challenge.client_public_key();
        let client_proof = *challenge.client_proof();

        wow_login_messages::version_3::CMD_AUTH_LOGON_PROOF_Client {
            client_public_key,
            client_proof,
            crc_hash: [0u8; 20],
            telemetry_keys: vec![],
            security_flag:
                wow_login_messages::version_3::CMD_AUTH_LOGON_PROOF_Client_SecurityFlag::None,
        }
        .write(&mut client)
        .unwrap();

        let server_proof = match ServerOpcodeMessage::read(&mut client).unwrap() {
            ServerOpcodeMessage::CMD_AUTH_LOGON_PROOF(CMD_AUTH_LOGON_PROOF_Server::Success {
                server_proof,
                ..
            }) => server_proof,
            other => panic!("expected proof success, got {other:?}"),
        };
        let srp_client = challenge.verify_server_proof(server_proof).unwrap();

        assert_eq!(
            srp_client.session_key(),
            store.saved.lock().unwrap().as_ref().unwrap()
        );

        wow_login_messages::version_8::CMD_REALM_LIST_Client {}
            .write(&mut client)
            .unwrap();
        match ServerOpcodeMessage::read(&mut client).unwrap() {
            ServerOpcodeMessage::CMD_REALM_LIST(reply) => {
                assert_eq!(reply.realms.len(), 1);
                assert_eq!(reply.realms[0].address, "127.0.0.1:8085");
                assert_eq!(reply.realms[0].number_of_characters_on_realm, 1);
            }
            other => panic!("expected realm list, got {other:?}"),
        }

        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn save_session_is_told_the_normalized_username_not_just_the_account_id() {
        let v = SrpVerifier::from_username_and_password(ns("TEST"), ns("PASSWORD"));
        let store = std::sync::Arc::new(InMemoryStore {
            account: LogonAccount {
                id: 7,
                salt: *v.salt(),
                verifier: *v.password_verifier(),
                banned: false,
            },
            username: "TEST".into(),
            saved: Mutex::new(None),
            usernames: Mutex::new(Vec::new()),
        });

        let (mut client, server_end) = UnixStream::pair().unwrap();
        let server_store = store.clone();
        let server = std::thread::spawn(move || {
            let mut s = server_end;
            let _ = handle_logon(&mut s, server_store.as_ref());
        });

        challenge_client("test").write(&mut client).unwrap();
        let (g, n, s_salt, server_pubkey) = match ServerOpcodeMessage::read(&mut client).unwrap() {
            ServerOpcodeMessage::CMD_AUTH_LOGON_CHALLENGE(
                CMD_AUTH_LOGON_CHALLENGE_Server::Success {
                    generator,
                    large_safe_prime,
                    salt,
                    server_public_key,
                    ..
                },
            ) => (generator, large_safe_prime, salt, server_public_key),
            other => panic!("expected challenge success, got {other:?}"),
        };
        let n: [u8; 32] = n.try_into().unwrap();
        let challenge = SrpClientChallenge::new(
            ns("TEST"),
            ns("PASSWORD"),
            g[0],
            n,
            PublicKey::from_le_bytes(server_pubkey).unwrap(),
            s_salt,
        );
        wow_login_messages::version_3::CMD_AUTH_LOGON_PROOF_Client {
            client_public_key: *challenge.client_public_key(),
            client_proof: *challenge.client_proof(),
            crc_hash: [0u8; 20],
            telemetry_keys: vec![],
            security_flag:
                wow_login_messages::version_3::CMD_AUTH_LOGON_PROOF_Client_SecurityFlag::None,
        }
        .write(&mut client)
        .unwrap();
        match ServerOpcodeMessage::read(&mut client).unwrap() {
            ServerOpcodeMessage::CMD_AUTH_LOGON_PROOF(CMD_AUTH_LOGON_PROOF_Server::Success {
                ..
            }) => {}
            other => panic!("expected proof success, got {other:?}"),
        }

        wow_login_messages::version_8::CMD_REALM_LIST_Client {}
            .write(&mut client)
            .unwrap();
        match ServerOpcodeMessage::read(&mut client).unwrap() {
            ServerOpcodeMessage::CMD_REALM_LIST(_) => {}
            other => panic!("expected realm list, got {other:?}"),
        }

        for method in ["bound_identity", "save_session", "realms"] {
            assert_eq!(
                store.told(method).as_deref(),
                Some("TEST"),
                "{method} must receive the normalized username — it is the only key valid across \
                 the realm-core and world databases, and {method} resolves a WORLD-shard id"
            );
        }
        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn a_world_side_id_is_re_resolved_from_the_username_never_carried_from_realm_core() {
        assert_eq!(
            world_account_id(7, "TEST", false, "world", || Ok(Some(4))).unwrap(),
            4,
            "the world shard's own id for the username, never realm-core's"
        );
    }

    #[test]
    fn an_unconfigured_realm_core_passes_the_id_through_without_a_lookup() {
        let id = world_account_id(7, "TEST", true, "world", || {
            panic!("no cross-database lookup may happen when realm-core is unconfigured")
        })
        .unwrap();
        assert_eq!(id, 7);
    }

    #[test]
    fn an_account_missing_on_the_world_shard_is_an_error_not_a_reused_realm_core_id() {
        let err = world_account_id(7, "TEST", false, "world", || Ok(None)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("TEST") && msg.contains("world"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn unknown_account_fails_cleanly() {
        let store = InMemoryStore {
            account: LogonAccount {
                id: 1,
                salt: [0; 32],
                verifier: [0; 32],
                banned: false,
            },
            username: "TEST".into(),
            saved: Mutex::new(None),
            usernames: Mutex::new(Vec::new()),
        };
        let (mut client, server_end) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut s = server_end;
            let _ = handle_logon(&mut s, &store);
        });

        challenge_client("NOBODY").write(&mut client).unwrap();
        match ServerOpcodeMessage::read(&mut client).unwrap() {
            ServerOpcodeMessage::CMD_AUTH_LOGON_CHALLENGE(
                CMD_AUTH_LOGON_CHALLENGE_Server::FailUnknownAccount,
            ) => {}
            other => panic!("expected FailUnknownAccount, got {other:?}"),
        }
        drop(client);
        server.join().unwrap();
    }

    fn provisioned_store(username: &str, password: &str, banned: bool) -> InMemoryStore {
        let v = SrpVerifier::from_username_and_password(ns(username), ns(password));
        InMemoryStore {
            account: LogonAccount {
                id: 1,
                salt: *v.salt(),
                verifier: *v.password_verifier(),
                banned,
            },
            username: username.into(),
            saved: Mutex::new(None),
            usernames: Mutex::new(Vec::new()),
        }
    }

    #[test]
    fn banned_account_challenge_replies_fail_banned() {
        let store = provisioned_store("TEST", "PASSWORD", true);
        let (mut client, server_end) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut s = server_end;
            let _ = handle_logon(&mut s, &store);
        });

        challenge_client("TEST").write(&mut client).unwrap();
        match ServerOpcodeMessage::read(&mut client).unwrap() {
            ServerOpcodeMessage::CMD_AUTH_LOGON_CHALLENGE(
                CMD_AUTH_LOGON_CHALLENGE_Server::FailBanned,
            ) => {}
            other => panic!("expected FailBanned, got {other:?}"),
        }
        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn unprovisioned_all_zero_verifier_is_rejected_as_unknown_account() {
        let store = InMemoryStore {
            account: LogonAccount {
                id: 1,
                salt: [9; 32],
                verifier: [0; 32],
                banned: false,
            },
            username: "TEST".into(),
            saved: Mutex::new(None),
            usernames: Mutex::new(Vec::new()),
        };
        let (mut client, server_end) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut s = server_end;
            let _ = handle_logon(&mut s, &store);
        });

        challenge_client("TEST").write(&mut client).unwrap();
        match ServerOpcodeMessage::read(&mut client).unwrap() {
            ServerOpcodeMessage::CMD_AUTH_LOGON_CHALLENGE(
                CMD_AUTH_LOGON_CHALLENGE_Server::FailUnknownAccount,
            ) => {}
            other => {
                panic!("expected FailUnknownAccount for the unprovisioned account, got {other:?}")
            }
        }
        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn wrong_password_proof_replies_fail_incorrect_password() {
        let store = provisioned_store("TEST", "PASSWORD", false);
        let (mut client, server_end) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut s = server_end;
            let _ = handle_logon(&mut s, &store);
        });

        challenge_client("TEST").write(&mut client).unwrap();
        let (g, n, s_salt, server_pubkey) = match ServerOpcodeMessage::read(&mut client).unwrap() {
            ServerOpcodeMessage::CMD_AUTH_LOGON_CHALLENGE(
                CMD_AUTH_LOGON_CHALLENGE_Server::Success {
                    generator,
                    large_safe_prime,
                    salt,
                    server_public_key,
                    ..
                },
            ) => (generator, large_safe_prime, salt, server_public_key),
            other => panic!("expected challenge success, got {other:?}"),
        };
        let n: [u8; 32] = n.try_into().unwrap();
        let challenge = SrpClientChallenge::new(
            ns("TEST"),
            ns("WRONGPASS"),
            g[0],
            n,
            PublicKey::from_le_bytes(server_pubkey).unwrap(),
            s_salt,
        );
        wow_login_messages::version_3::CMD_AUTH_LOGON_PROOF_Client {
            client_public_key: *challenge.client_public_key(),
            client_proof: *challenge.client_proof(),
            crc_hash: [0u8; 20],
            telemetry_keys: vec![],
            security_flag:
                wow_login_messages::version_3::CMD_AUTH_LOGON_PROOF_Client_SecurityFlag::None,
        }
        .write(&mut client)
        .unwrap();

        match ServerOpcodeMessage::read(&mut client).unwrap() {
            ServerOpcodeMessage::CMD_AUTH_LOGON_PROOF(
                CMD_AUTH_LOGON_PROOF_Server::FailIncorrectPassword,
            ) => {}
            other => panic!("expected FailIncorrectPassword, got {other:?}"),
        }
        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn proof_before_a_challenge_is_a_protocol_error() {
        let store = provisioned_store("TEST", "PASSWORD", false);
        let (mut client, server_end) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut s = server_end;
            handle_logon(&mut s, &store)
        });

        wow_login_messages::version_3::CMD_AUTH_LOGON_PROOF_Client {
            client_public_key: [1u8; 32],
            client_proof: [0u8; 20],
            crc_hash: [0u8; 20],
            telemetry_keys: vec![],
            security_flag:
                wow_login_messages::version_3::CMD_AUTH_LOGON_PROOF_Client_SecurityFlag::None,
        }
        .write(&mut client)
        .unwrap();

        let result = server.join().unwrap();
        let err = result.expect_err("an out-of-order proof must be a protocol error");
        assert!(
            err.to_string().contains("before a challenge"),
            "unexpected error: {err:#}"
        );
        drop(client);
    }

    #[test]
    fn realm_list_before_a_completed_proof_is_empty() {
        let store = provisioned_store("TEST", "PASSWORD", false);
        let (mut client, server_end) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut s = server_end;
            let _ = handle_logon(&mut s, &store);
        });

        wow_login_messages::version_8::CMD_REALM_LIST_Client {}
            .write(&mut client)
            .unwrap();
        match ServerOpcodeMessage::read(&mut client).unwrap() {
            ServerOpcodeMessage::CMD_REALM_LIST(reply) => {
                assert!(reply.realms.is_empty(), "no realms before authentication");
            }
            other => panic!("expected an empty realm list, got {other:?}"),
        }
        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn to_realm_maps_the_realm_type_code() {
        fn realm(realm_type: u32) -> RealmInfo {
            RealmInfo {
                id: 1,
                name: "LyraCore".into(),
                address: "127.0.0.1:8085".into(),
                realm_type,
                population: 0.0,
                number_of_characters: 0,
            }
        }
        assert_eq!(to_realm(&realm(1)).realm_type, RealmType::PlayerVsPlayer);
        assert_eq!(to_realm(&realm(6)).realm_type, RealmType::Roleplaying);
        assert_eq!(
            to_realm(&realm(8)).realm_type,
            RealmType::RoleplayingPlayerVsPlayer
        );
        assert_eq!(
            to_realm(&realm(0)).realm_type,
            RealmType::PlayerVsEnvironment
        );
        assert_eq!(
            to_realm(&realm(99)).realm_type,
            RealmType::PlayerVsEnvironment
        );
    }

    fn handshake_as(
        stored_account: &str,
        password: &str,
        typed_account: &str,
    ) -> std::sync::Arc<InMemoryStore> {
        let store = std::sync::Arc::new(provisioned_store(stored_account, password, false));
        let (mut client, server_end) = UnixStream::pair().unwrap();
        let server_store = store.clone();
        let server = std::thread::spawn(move || {
            let mut s = server_end;
            handle_logon(&mut s, server_store.as_ref())
        });

        challenge_client(typed_account).write(&mut client).unwrap();
        let (g, n, s_salt, server_pubkey) = match ServerOpcodeMessage::read(&mut client).unwrap() {
            ServerOpcodeMessage::CMD_AUTH_LOGON_CHALLENGE(
                CMD_AUTH_LOGON_CHALLENGE_Server::Success {
                    generator,
                    large_safe_prime,
                    salt,
                    server_public_key,
                    ..
                },
            ) => (generator, large_safe_prime, salt, server_public_key),
            other => panic!(
                "account {typed_account:?} did not resolve to the account provisioned as \
                 {stored_account:?}: got {other:?}"
            ),
        };
        let n: [u8; 32] = n.try_into().unwrap();
        let challenge = SrpClientChallenge::new(
            ns(&typed_account.to_uppercase()),
            ns(password),
            g[0],
            n,
            PublicKey::from_le_bytes(server_pubkey).unwrap(),
            s_salt,
        );
        wow_login_messages::version_3::CMD_AUTH_LOGON_PROOF_Client {
            client_public_key: *challenge.client_public_key(),
            client_proof: *challenge.client_proof(),
            crc_hash: [0u8; 20],
            telemetry_keys: vec![],
            security_flag:
                wow_login_messages::version_3::CMD_AUTH_LOGON_PROOF_Client_SecurityFlag::None,
        }
        .write(&mut client)
        .unwrap();

        match ServerOpcodeMessage::read(&mut client).unwrap() {
            ServerOpcodeMessage::CMD_AUTH_LOGON_PROOF(CMD_AUTH_LOGON_PROOF_Server::Success {
                ..
            }) => {}
            other => panic!("proof for {typed_account:?} was rejected: {other:?}"),
        }

        wow_login_messages::version_8::CMD_REALM_LIST_Client {}
            .write(&mut client)
            .unwrap();
        match ServerOpcodeMessage::read(&mut client).unwrap() {
            ServerOpcodeMessage::CMD_REALM_LIST(reply) => assert!(
                !reply.realms.is_empty(),
                "an authenticated {typed_account:?} must be shown the realm"
            ),
            other => panic!("expected the realm list, got {other:?}"),
        }
        drop(client);
        server.join().unwrap().expect("the session ends cleanly");
        store
    }

    #[test]
    fn an_account_provisioned_uppercase_authenticates_whatever_case_the_player_types() {
        for typed in ["tester", "TESTER", "TeStEr", "tESTER"] {
            let store = handshake_as("TESTER", "PASSWORD", typed);
            assert_eq!(
                store.told("save_session").as_deref(),
                Some("TESTER"),
                "typing {typed:?} must resolve to the canonical uppercased account name before any \
                 store call — a store keyed on the raw typed name writes a second session row per \
                 capitalization"
            );
            assert_eq!(
                store.told("realms").as_deref(),
                Some("TESTER"),
                "the realm-list lookup must use the canonical name too"
            );
        }
    }

    #[test]
    fn provisioning_the_same_credentials_twice_yields_different_salts_that_both_authenticate() {
        let (user, pass) =
            crate::provision_cli::normalize_provision_credentials("tester", b"password")
                .expect("the fixture credentials are valid");
        let first = SrpVerifier::from_username_and_password(user.clone(), pass.clone());
        let second = SrpVerifier::from_username_and_password(user, pass);
        assert_ne!(
            first.salt(),
            second.salt(),
            "provisioning must draw a FRESH random salt each time. A fixed salt makes the stored \
             verifier a password-equivalent constant shared by every deployment of this code."
        );
        assert_ne!(
            first.password_verifier(),
            second.password_verifier(),
            "different salts must produce different verifiers, or the salt is not reaching the \
             derivation at all"
        );

        for (which, v) in [("first", &first), ("second", &second)] {
            let store = std::sync::Arc::new(InMemoryStore {
                account: LogonAccount {
                    id: 1,
                    salt: *v.salt(),
                    verifier: *v.password_verifier(),
                    banned: false,
                },
                username: "TESTER".into(),
                saved: Mutex::new(None),
                usernames: Mutex::new(Vec::new()),
            });
            let (mut client, server_end) = UnixStream::pair().unwrap();
            let server_store = store.clone();
            let server = std::thread::spawn(move || {
                let mut s = server_end;
                handle_logon(&mut s, server_store.as_ref())
            });

            challenge_client("tester").write(&mut client).unwrap();
            let (g, n, s_salt, server_pubkey) =
                match ServerOpcodeMessage::read(&mut client).unwrap() {
                    ServerOpcodeMessage::CMD_AUTH_LOGON_CHALLENGE(
                        CMD_AUTH_LOGON_CHALLENGE_Server::Success {
                            generator,
                            large_safe_prime,
                            salt,
                            server_public_key,
                            ..
                        },
                    ) => (generator, large_safe_prime, salt, server_public_key),
                    other => panic!("{which} provisioning was not accepted: {other:?}"),
                };
            assert_eq!(
                s_salt,
                *v.salt(),
                "the challenge must echo the salt provisioning stored, or the client derives its \
                 proof from the wrong one"
            );
            let n: [u8; 32] = n.try_into().unwrap();
            let challenge = SrpClientChallenge::new(
                ns("TESTER"),
                ns("PASSWORD"),
                g[0],
                n,
                PublicKey::from_le_bytes(server_pubkey).unwrap(),
                s_salt,
            );
            wow_login_messages::version_3::CMD_AUTH_LOGON_PROOF_Client {
                client_public_key: *challenge.client_public_key(),
                client_proof: *challenge.client_proof(),
                crc_hash: [0u8; 20],
                telemetry_keys: vec![],
                security_flag:
                    wow_login_messages::version_3::CMD_AUTH_LOGON_PROOF_Client_SecurityFlag::None,
            }
            .write(&mut client)
            .unwrap();
            match ServerOpcodeMessage::read(&mut client).unwrap() {
                ServerOpcodeMessage::CMD_AUTH_LOGON_PROOF(
                    CMD_AUTH_LOGON_PROOF_Server::Success { .. },
                ) => {}
                other => panic!(
                    "the {which} independently-salted provisioning must authenticate the same \
                     password, got {other:?}"
                ),
            }
            assert!(
                store.saved.lock().unwrap().is_some(),
                "a completed proof must persist the session key K"
            );
            drop(client);
            server.join().unwrap().expect("the session ends cleanly");
        }
    }

    #[test]
    fn credentials_from_the_provisioning_path_authenticate_through_a_real_handshake() {
        let (user, pass) =
            crate::provision_cli::normalize_provision_credentials("tester", b"my pass phrase")
                .expect("an ordinary username/password must be accepted by provisioning");
        let verifier = SrpVerifier::from_username_and_password(user.clone(), pass);

        for typed in ["TESTER", "tester", "TeStEr"] {
            let store = std::sync::Arc::new(InMemoryStore {
                account: LogonAccount {
                    id: 1,
                    salt: *verifier.salt(),
                    verifier: *verifier.password_verifier(),
                    banned: false,
                },
                username: user.as_ref().to_string(),
                saved: Mutex::new(None),
                usernames: Mutex::new(Vec::new()),
            });
            let (mut client, server_end) = UnixStream::pair().unwrap();
            let server_store = store.clone();
            let server = std::thread::spawn(move || {
                let mut s = server_end;
                handle_logon(&mut s, server_store.as_ref())
            });

            challenge_client(typed).write(&mut client).unwrap();
            let (g, n, s_salt, server_pubkey) =
                match ServerOpcodeMessage::read(&mut client).unwrap() {
                    ServerOpcodeMessage::CMD_AUTH_LOGON_CHALLENGE(
                        CMD_AUTH_LOGON_CHALLENGE_Server::Success {
                            generator,
                            large_safe_prime,
                            salt,
                            server_public_key,
                            ..
                        },
                    ) => (generator, large_safe_prime, salt, server_public_key),
                    other => panic!(
                    "an account provisioned as {:?} was not found when the player typed {typed:?} \
                     — the provisioning and logon tiers disagree about normalization, which \
                     presents to every affected player as 'unknown account'. Got {other:?}",
                    user.as_ref()
                ),
                };
            let n: [u8; 32] = n.try_into().unwrap();
            let challenge = SrpClientChallenge::new(
                ns(typed),
                ns("MY PASS PHRASE"),
                g[0],
                n,
                PublicKey::from_le_bytes(server_pubkey).unwrap(),
                s_salt,
            );
            wow_login_messages::version_3::CMD_AUTH_LOGON_PROOF_Client {
                client_public_key: *challenge.client_public_key(),
                client_proof: *challenge.client_proof(),
                crc_hash: [0u8; 20],
                telemetry_keys: vec![],
                security_flag:
                    wow_login_messages::version_3::CMD_AUTH_LOGON_PROOF_Client_SecurityFlag::None,
            }
            .write(&mut client)
            .unwrap();
            match ServerOpcodeMessage::read(&mut client).unwrap() {
                ServerOpcodeMessage::CMD_AUTH_LOGON_PROOF(
                    CMD_AUTH_LOGON_PROOF_Server::Success { .. },
                ) => {}
                other => panic!(
                    "the password provisioning derived its verifier from must authenticate when \
                     typed as {typed:?}; got {other:?}"
                ),
            }
            drop(client);
            server.join().unwrap().expect("the session ends cleanly");
        }
    }

    #[test]
    fn normalization_folds_case_only_and_never_merges_two_different_accounts() {
        let store = provisioned_store("TESTER", "PASSWORD", false);
        let (mut client, server_end) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut s = server_end;
            let _ = handle_logon(&mut s, &store);
        });
        challenge_client("tester2").write(&mut client).unwrap();
        match ServerOpcodeMessage::read(&mut client).unwrap() {
            ServerOpcodeMessage::CMD_AUTH_LOGON_CHALLENGE(
                CMD_AUTH_LOGON_CHALLENGE_Server::FailUnknownAccount,
            ) => {}
            other => panic!("`tester2` must not resolve to the `TESTER` account, got {other:?}"),
        }
        drop(client);
        server.join().unwrap();
    }

    struct FailingStore {
        on: &'static str,
        account: LogonAccount,
        username: String,
    }

    impl LogonStore for FailingStore {
        fn account(&self, username: &str) -> Result<Option<LogonAccount>> {
            if self.on == "account" {
                return Err(anyhow!("realm-core unreachable"));
            }
            Ok((username == self.username).then(|| self.account.clone()))
        }
        fn bound_identity(&self, _account_id: u64, _username: &str) -> Result<[u8; 32]> {
            if self.on == "bound_identity" {
                return Err(anyhow!("realm-core unreachable"));
            }
            Ok([7u8; 32])
        }
        fn save_session(
            &self,
            _id: u64,
            _username: &str,
            _key: &[u8; 40],
            _b: [u8; 32],
        ) -> Result<()> {
            if self.on == "save_session" {
                return Err(anyhow!("realm-core unreachable"));
            }
            Ok(())
        }
        fn realms(&self, _account_id: u64, _username: &str) -> Result<Vec<RealmInfo>> {
            if self.on == "realms" {
                return Err(anyhow!("realm-core unreachable"));
            }
            Ok(Vec::new())
        }
    }

    fn failing_store(on: &'static str) -> FailingStore {
        let v = SrpVerifier::from_username_and_password(ns("TEST"), ns("PASSWORD"));
        FailingStore {
            on,
            account: LogonAccount {
                id: 1,
                salt: *v.salt(),
                verifier: *v.password_verifier(),
                banned: false,
            },
            username: "TEST".into(),
        }
    }

    #[test]
    fn a_store_failure_during_the_challenge_is_an_error_never_an_unknown_account_reply() {
        let store = failing_store("account");
        let (mut client, server_end) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut s = server_end;
            handle_logon(&mut s, &store)
        });
        challenge_client("TEST").write(&mut client).unwrap();

        let seen = ServerOpcodeMessage::read(&mut client);
        assert!(
            seen.is_err(),
            "a store outage must not answer the challenge at all; the client was told {seen:?}"
        );
        let err = server
            .join()
            .unwrap()
            .expect_err("a store outage must fail the logon session");
        assert!(
            format!("{err:#}").contains("realm-core unreachable"),
            "the store's own message must reach the operator's log: {err:#}"
        );
        drop(client);
    }

    #[test]
    fn a_realm_list_read_failure_after_a_good_proof_fails_loudly_not_with_an_empty_list() {
        let store = failing_store("realms");
        let (mut client, server_end) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut s = server_end;
            handle_logon(&mut s, &store)
        });

        challenge_client("TEST").write(&mut client).unwrap();
        let (g, n, s_salt, server_pubkey) = match ServerOpcodeMessage::read(&mut client).unwrap() {
            ServerOpcodeMessage::CMD_AUTH_LOGON_CHALLENGE(
                CMD_AUTH_LOGON_CHALLENGE_Server::Success {
                    generator,
                    large_safe_prime,
                    salt,
                    server_public_key,
                    ..
                },
            ) => (generator, large_safe_prime, salt, server_public_key),
            other => panic!("expected challenge success, got {other:?}"),
        };
        let n: [u8; 32] = n.try_into().unwrap();
        let challenge = SrpClientChallenge::new(
            ns("TEST"),
            ns("PASSWORD"),
            g[0],
            n,
            PublicKey::from_le_bytes(server_pubkey).unwrap(),
            s_salt,
        );
        wow_login_messages::version_3::CMD_AUTH_LOGON_PROOF_Client {
            client_public_key: *challenge.client_public_key(),
            client_proof: *challenge.client_proof(),
            crc_hash: [0u8; 20],
            telemetry_keys: vec![],
            security_flag:
                wow_login_messages::version_3::CMD_AUTH_LOGON_PROOF_Client_SecurityFlag::None,
        }
        .write(&mut client)
        .unwrap();
        match ServerOpcodeMessage::read(&mut client).unwrap() {
            ServerOpcodeMessage::CMD_AUTH_LOGON_PROOF(CMD_AUTH_LOGON_PROOF_Server::Success {
                ..
            }) => {}
            other => panic!("the proof should have been accepted, got {other:?}"),
        }
        wow_login_messages::version_8::CMD_REALM_LIST_Client {}
            .write(&mut client)
            .unwrap();
        let err = server
            .join()
            .unwrap()
            .expect_err("a failed realm-list read must end the session, not send an empty list");
        assert!(
            format!("{err:#}").contains("realm-core unreachable"),
            "{err:#}"
        );
        drop(client);
    }

    #[test]
    fn a_replayed_proof_cannot_authenticate_a_second_time() {
        let store = provisioned_store("TEST", "PASSWORD", false);
        let (mut client, server_end) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut s = server_end;
            handle_logon(&mut s, &store)
        });

        challenge_client("TEST").write(&mut client).unwrap();
        let (g, n, s_salt, server_pubkey) = match ServerOpcodeMessage::read(&mut client).unwrap() {
            ServerOpcodeMessage::CMD_AUTH_LOGON_CHALLENGE(
                CMD_AUTH_LOGON_CHALLENGE_Server::Success {
                    generator,
                    large_safe_prime,
                    salt,
                    server_public_key,
                    ..
                },
            ) => (generator, large_safe_prime, salt, server_public_key),
            other => panic!("expected challenge success, got {other:?}"),
        };
        let n: [u8; 32] = n.try_into().unwrap();
        let challenge = SrpClientChallenge::new(
            ns("TEST"),
            ns("PASSWORD"),
            g[0],
            n,
            PublicKey::from_le_bytes(server_pubkey).unwrap(),
            s_salt,
        );
        let proof = wow_login_messages::version_3::CMD_AUTH_LOGON_PROOF_Client {
            client_public_key: *challenge.client_public_key(),
            client_proof: *challenge.client_proof(),
            crc_hash: [0u8; 20],
            telemetry_keys: vec![],
            security_flag:
                wow_login_messages::version_3::CMD_AUTH_LOGON_PROOF_Client_SecurityFlag::None,
        };
        proof.write(&mut client).unwrap();
        match ServerOpcodeMessage::read(&mut client).unwrap() {
            ServerOpcodeMessage::CMD_AUTH_LOGON_PROOF(CMD_AUTH_LOGON_PROOF_Server::Success {
                ..
            }) => {}
            other => panic!("the first proof must succeed, got {other:?}"),
        }

        proof.write(&mut client).unwrap();
        let err = server
            .join()
            .unwrap()
            .expect_err("a replayed proof must be refused, not re-authenticated");
        assert!(
            err.to_string().contains("before a challenge"),
            "the replay must be rejected as having no pending SRP state; got: {err:#}"
        );
        drop(client);
    }

    fn fail_proof(client: &mut UnixStream, username: &str, wrong_password: &str) {
        challenge_client(username).write(&mut *client).unwrap();
        let (g, n, s_salt, server_pubkey) = match ServerOpcodeMessage::read(&mut *client).unwrap() {
            ServerOpcodeMessage::CMD_AUTH_LOGON_CHALLENGE(
                CMD_AUTH_LOGON_CHALLENGE_Server::Success {
                    generator,
                    large_safe_prime,
                    salt,
                    server_public_key,
                    ..
                },
            ) => (generator, large_safe_prime, salt, server_public_key),
            other => panic!("expected challenge success, got {other:?}"),
        };
        let n: [u8; 32] = n.try_into().unwrap();
        let challenge = SrpClientChallenge::new(
            ns(username),
            ns(wrong_password),
            g[0],
            n,
            PublicKey::from_le_bytes(server_pubkey).unwrap(),
            s_salt,
        );
        wow_login_messages::version_3::CMD_AUTH_LOGON_PROOF_Client {
            client_public_key: *challenge.client_public_key(),
            client_proof: *challenge.client_proof(),
            crc_hash: [0u8; 20],
            telemetry_keys: vec![],
            security_flag:
                wow_login_messages::version_3::CMD_AUTH_LOGON_PROOF_Client_SecurityFlag::None,
        }
        .write(&mut *client)
        .unwrap();
        match ServerOpcodeMessage::read(&mut *client).unwrap() {
            ServerOpcodeMessage::CMD_AUTH_LOGON_PROOF(
                CMD_AUTH_LOGON_PROOF_Server::FailIncorrectPassword,
            ) => {}
            other => panic!("expected FailIncorrectPassword, got {other:?}"),
        }
    }

    fn socket_is_closed(client: &mut UnixStream) -> bool {
        let mut byte = [0u8];
        matches!(client.read(&mut byte), Ok(0))
    }

    #[test]
    fn the_third_failed_proof_closes_the_connection() {
        let store = provisioned_store("TEST", "PASSWORD", false);
        let (mut client, server_end) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut s = server_end;
            handle_logon(&mut s, &store)
        });

        for _ in 0..limiter::ATTEMPTS_PER_CONNECTION {
            fail_proof(&mut client, "TEST", "WRONGPASS");
        }

        assert!(
            socket_is_closed(&mut client),
            "the client still saw its last FailIncorrectPassword, then lost the socket"
        );
        let err = server
            .join()
            .unwrap()
            .expect_err("the handler ends with the limiter's verdict");
        assert!(err.to_string().contains("logon attempts"), "{err:#}");
    }

    #[test]
    fn the_third_refused_challenge_is_answered_before_the_connection_closes() {
        let store = provisioned_store("TEST", "PASSWORD", false);
        let (mut client, mut stream) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || handle_logon(&mut stream, &store));
        for _ in 0..limiter::ATTEMPTS_PER_CONNECTION {
            challenge_client("UNKNOWN").write(&mut client).unwrap();
            assert!(matches!(
                ServerOpcodeMessage::read(&mut client).unwrap(),
                ServerOpcodeMessage::CMD_AUTH_LOGON_CHALLENGE(
                    CMD_AUTH_LOGON_CHALLENGE_Server::FailUnknownAccount
                )
            ));
        }
        assert!(socket_is_closed(&mut client));
        let error = server.join().unwrap().unwrap_err();
        assert_eq!(
            error.downcast_ref::<limiter::LogonRefusal>(),
            Some(&limiter::LogonRefusal::AttemptsExhausted)
        );
    }

    struct FailedReply {
        input: std::io::Cursor<Vec<u8>>,
        opcode: u8,
    }

    impl Read for FailedReply {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            self.input.read(bytes)
        }
    }

    impl Write for FailedReply {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.first() == Some(&self.opcode) {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl IoDeadline for FailedReply {
        fn set_read_timeout(&self, _: Option<Duration>) -> std::io::Result<()> {
            Ok(())
        }

        fn set_write_timeout(&self, _: Option<Duration>) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn failed_proof_replies_still_spend_the_address_budget() {
        let store = provisioned_store("TEST", "PASSWORD", false);
        let limiter = LogonLimiter::new();
        let peer = std::net::Ipv4Addr::LOCALHOST.into();
        let mut packets = Vec::new();
        challenge_client("TEST").write(&mut packets).unwrap();
        wow_login_messages::version_3::CMD_AUTH_LOGON_PROOF_Client {
            client_public_key: [0; 32],
            client_proof: [0; 20],
            crc_hash: [0; 20],
            telemetry_keys: vec![],
            security_flag:
                wow_login_messages::version_3::CMD_AUTH_LOGON_PROOF_Client_SecurityFlag::None,
        }
        .write(&mut packets)
        .unwrap();

        for _ in 0..limiter::FAILURES_PER_WINDOW {
            let mut connection = limiter.admit(peer, Instant::now()).unwrap();
            let mut stream = FailedReply {
                input: std::io::Cursor::new(packets.clone()),
                opcode: 1,
            };
            let error = handle_logon_with_deadline(
                &mut stream,
                &store,
                &mut PreAuthDeadline::after(LOGON_AUTH_DEADLINE),
                &mut connection,
            )
            .expect_err("the failed proof reply cannot be delivered");
            assert_eq!(
                error.downcast_ref::<std::io::Error>().unwrap().kind(),
                std::io::ErrorKind::BrokenPipe
            );
        }
        assert!(matches!(
            limiter.admit(peer, Instant::now()),
            Err(limiter::LogonRefusal::TooManyFailures)
        ));
        assert!(store.saved.lock().unwrap().is_none());
    }

    #[test]
    fn failed_challenge_replies_still_spend_the_address_budget() {
        let mut unprovisioned = provisioned_store("TEST", "PASSWORD", false);
        unprovisioned.account.verifier = [0; 32];
        for (username, store) in [
            ("UNKNOWN", provisioned_store("TEST", "PASSWORD", false)),
            ("TEST", provisioned_store("TEST", "PASSWORD", true)),
            ("TEST", unprovisioned),
        ] {
            let limiter = LogonLimiter::new();
            let peer = std::net::Ipv4Addr::LOCALHOST.into();
            let mut packet = Vec::new();
            challenge_client(username).write(&mut packet).unwrap();
            for _ in 0..limiter::FAILURES_PER_WINDOW {
                let mut connection = limiter.admit(peer, Instant::now()).unwrap();
                let mut stream = FailedReply {
                    input: std::io::Cursor::new(packet.clone()),
                    opcode: 0,
                };
                let error = handle_logon_with_deadline(
                    &mut stream,
                    &store,
                    &mut PreAuthDeadline::after(LOGON_AUTH_DEADLINE),
                    &mut connection,
                )
                .expect_err("the challenge refusal cannot be delivered");
                assert_eq!(
                    error.downcast_ref::<std::io::Error>().unwrap().kind(),
                    std::io::ErrorKind::BrokenPipe
                );
            }
            assert!(matches!(
                limiter.admit(peer, Instant::now()),
                Err(limiter::LogonRefusal::TooManyFailures)
            ));
        }
    }

    #[test]
    fn a_failed_proof_is_answered_after_the_failure_delay() {
        let store = provisioned_store("TEST", "PASSWORD", false);
        let (mut client, server_end) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut s = server_end;
            let _ = handle_logon(&mut s, &store);
        });

        let started = Instant::now();
        fail_proof(&mut client, "TEST", "WRONGPASS");
        assert!(
            started.elapsed() >= PROOF_FAILURE_DELAY,
            "a wrong proof was answered in {:?}",
            started.elapsed()
        );

        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn an_address_that_exhausted_its_window_is_closed_at_the_next_challenge() {
        let store = provisioned_store("TEST", "PASSWORD", false);
        let limiter = LogonLimiter::new();
        let peer: std::net::IpAddr = std::net::Ipv4Addr::new(10, 0, 0, 1).into();
        let now = Instant::now();
        let mut connection = limiter.admit(peer, now).unwrap();
        for _ in 0..limiter::FAILURES_PER_WINDOW {
            let mut other = limiter.admit(peer, now).unwrap();
            other.start_attempt(now).unwrap();
            other.record_failure(now).unwrap();
        }
        let (mut client, server_end) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut s = server_end;
            let mut deadline = PreAuthDeadline::after(LOGON_AUTH_DEADLINE);
            handle_logon_with_deadline(&mut s, &store, &mut deadline, &mut connection)
        });

        challenge_client("TEST").write(&mut client).unwrap();
        assert!(
            socket_is_closed(&mut client),
            "no challenge reply: the connection is closed before any SRP math"
        );
        let err = server.join().unwrap().expect_err("the limiter's verdict");
        assert_eq!(
            err.downcast_ref::<limiter::LogonRefusal>(),
            Some(&limiter::LogonRefusal::TooManyFailures)
        );
    }

    fn prove(client: &mut UnixStream, username: &str, password: &str) {
        challenge_client(username).write(&mut *client).unwrap();
        let (g, n, s_salt, server_pubkey) = match ServerOpcodeMessage::read(&mut *client).unwrap() {
            ServerOpcodeMessage::CMD_AUTH_LOGON_CHALLENGE(
                CMD_AUTH_LOGON_CHALLENGE_Server::Success {
                    generator,
                    large_safe_prime,
                    salt,
                    server_public_key,
                    ..
                },
            ) => (generator, large_safe_prime, salt, server_public_key),
            other => panic!("expected challenge success, got {other:?}"),
        };
        let n: [u8; 32] = n.try_into().unwrap();
        let challenge = SrpClientChallenge::new(
            ns(username),
            ns(password),
            g[0],
            n,
            PublicKey::from_le_bytes(server_pubkey).unwrap(),
            s_salt,
        );
        wow_login_messages::version_3::CMD_AUTH_LOGON_PROOF_Client {
            client_public_key: *challenge.client_public_key(),
            client_proof: *challenge.client_proof(),
            crc_hash: [0u8; 20],
            telemetry_keys: vec![],
            security_flag:
                wow_login_messages::version_3::CMD_AUTH_LOGON_PROOF_Client_SecurityFlag::None,
        }
        .write(&mut *client)
        .unwrap();
        match ServerOpcodeMessage::read(&mut *client).unwrap() {
            ServerOpcodeMessage::CMD_AUTH_LOGON_PROOF(CMD_AUTH_LOGON_PROOF_Server::Success {
                ..
            }) => {}
            other => panic!("the proof must succeed, got {other:?}"),
        }
    }

    #[test]
    fn a_silent_connection_is_closed_at_the_pre_auth_deadline() {
        let store = provisioned_store("TEST", "PASSWORD", false);
        let (mut client, server_end) = UnixStream::pair().unwrap();
        let mut deadline = PreAuthDeadline::after(Duration::from_millis(200));
        let server = std::thread::spawn(move || {
            let mut s = server_end;
            handle_logon_with_deadline(&mut s, &store, &mut deadline, &mut loopback_connection())
        });

        let err = server
            .join()
            .unwrap()
            .expect_err("a silent peer must be cut at the deadline");
        assert!(
            err.to_string().contains("total pre-auth deadline"),
            "{err:#}"
        );
        let mut byte = [0u8];
        assert_eq!(
            client.read(&mut byte).unwrap(),
            0,
            "the socket must be closed"
        );
    }

    #[test]
    fn repeated_pre_auth_requests_cannot_extend_the_pre_auth_deadline() {
        let store = provisioned_store("TEST", "PASSWORD", false);
        let mut input = Vec::new();
        for _ in 0..64 {
            wow_login_messages::version_8::CMD_REALM_LIST_Client {}
                .write(&mut input)
                .unwrap();
        }
        let start = Instant::now();
        let clock = ManualClock(Rc::new(Cell::new(start)));
        let mut deadline =
            PreAuthDeadline::with_clock(start + Duration::from_millis(100), clock.clone());
        let mut stream = AdvancingStream {
            input: Cursor::new(input),
            output: Vec::new(),
            clock,
            per_read: Duration::from_millis(1),
            per_write: Duration::ZERO,
            write_timeout_calls: Rc::new(Cell::new(0)),
        };

        let error = handle_logon_with_deadline(
            &mut stream,
            &store,
            &mut deadline,
            &mut loopback_connection(),
        )
        .expect_err("complete pre-auth requests must share one total deadline");

        assert!(
            error.to_string().contains("pre-auth I/O deadline"),
            "{error:#}"
        );
        assert!(
            !stream.output.is_empty(),
            "at least one complete request must make progress before the deadline"
        );
    }

    #[test]
    fn challenge_response_write_uses_the_absolute_pre_auth_deadline() {
        let store = provisioned_store("TEST", "PASSWORD", false);
        let mut input = Vec::new();
        challenge_client("TEST").write(&mut input).unwrap();
        let start = Instant::now();
        let clock = ManualClock(Rc::new(Cell::new(start)));
        let write_timeout_calls = Rc::new(Cell::new(0));
        let mut deadline =
            PreAuthDeadline::with_clock(start + Duration::from_millis(10), clock.clone());
        let mut stream = AdvancingStream {
            input: Cursor::new(input),
            output: Vec::new(),
            clock,
            per_read: Duration::ZERO,
            per_write: Duration::from_millis(20),
            write_timeout_calls: write_timeout_calls.clone(),
        };

        let error = handle_logon_with_deadline(
            &mut stream,
            &store,
            &mut deadline,
            &mut loopback_connection(),
        )
        .expect_err("the challenge reply must not write past the total deadline");

        assert_eq!(
            write_timeout_calls.get(),
            1,
            "handle_challenge must write through the pre-auth I/O wrapper"
        );
        assert!(
            error.to_string().contains("pre-auth I/O deadline"),
            "{error:#}"
        );
        assert!(
            !stream.output.is_empty(),
            "the fake advances time after accepting the challenge reply bytes"
        );
    }

    #[test]
    fn the_pre_auth_deadline_ends_once_the_proof_succeeds() {
        let store = provisioned_store("TEST", "PASSWORD", false);
        let (mut client, server_end) = UnixStream::pair().unwrap();
        let mut deadline = PreAuthDeadline::after(Duration::from_millis(200));
        let server = std::thread::spawn(move || {
            let mut s = server_end;
            handle_logon_with_deadline(&mut s, &store, &mut deadline, &mut loopback_connection())
        });

        prove(&mut client, "TEST", "PASSWORD");
        std::thread::sleep(std::time::Duration::from_millis(500));
        wow_login_messages::version_8::CMD_REALM_LIST_Client {}
            .write(&mut client)
            .unwrap();
        match ServerOpcodeMessage::read(&mut client).unwrap() {
            ServerOpcodeMessage::CMD_REALM_LIST(reply) => assert_eq!(reply.realms.len(), 1),
            other => panic!("expected realm list, got {other:?}"),
        }
        drop(client);
        server.join().unwrap().unwrap();
    }

    #[test]
    fn a_second_challenge_replaces_the_pending_srp_state_rather_than_stranding_the_client() {
        let store = provisioned_store("TEST", "PASSWORD", false);
        let (mut client, server_end) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut s = server_end;
            handle_logon(&mut s, &store)
        });

        challenge_client("TEST").write(&mut client).unwrap();
        let first_pubkey = match ServerOpcodeMessage::read(&mut client).unwrap() {
            ServerOpcodeMessage::CMD_AUTH_LOGON_CHALLENGE(
                CMD_AUTH_LOGON_CHALLENGE_Server::Success {
                    server_public_key, ..
                },
            ) => server_public_key,
            other => panic!("expected challenge success, got {other:?}"),
        };

        challenge_client("TEST").write(&mut client).unwrap();
        let (g, n, s_salt, server_pubkey) = match ServerOpcodeMessage::read(&mut client).unwrap() {
            ServerOpcodeMessage::CMD_AUTH_LOGON_CHALLENGE(
                CMD_AUTH_LOGON_CHALLENGE_Server::Success {
                    generator,
                    large_safe_prime,
                    salt,
                    server_public_key,
                    ..
                },
            ) => (generator, large_safe_prime, salt, server_public_key),
            other => panic!("the second challenge must also be answered, got {other:?}"),
        };
        assert_ne!(
            server_pubkey, first_pubkey,
            "each challenge must mint fresh SRP6 material — reusing the ephemeral key across \
             challenges would make the handshake replayable"
        );

        let n: [u8; 32] = n.try_into().unwrap();
        let challenge = SrpClientChallenge::new(
            ns("TEST"),
            ns("PASSWORD"),
            g[0],
            n,
            PublicKey::from_le_bytes(server_pubkey).unwrap(),
            s_salt,
        );
        wow_login_messages::version_3::CMD_AUTH_LOGON_PROOF_Client {
            client_public_key: *challenge.client_public_key(),
            client_proof: *challenge.client_proof(),
            crc_hash: [0u8; 20],
            telemetry_keys: vec![],
            security_flag:
                wow_login_messages::version_3::CMD_AUTH_LOGON_PROOF_Client_SecurityFlag::None,
        }
        .write(&mut client)
        .unwrap();
        match ServerOpcodeMessage::read(&mut client).unwrap() {
            ServerOpcodeMessage::CMD_AUTH_LOGON_PROOF(CMD_AUTH_LOGON_PROOF_Server::Success {
                ..
            }) => {}
            other => panic!(
                "a proof against the SECOND challenge must be accepted; the handler kept stale SRP \
                 state and answered {other:?}"
            ),
        }
        drop(client);
        server.join().unwrap().expect("the session ends cleanly");
    }

    #[test]
    fn an_over_long_account_name_is_declined_cleanly_before_any_srp_math() {
        let store = provisioned_store("TEST", "PASSWORD", false);
        let (mut client, server_end) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut s = server_end;
            let _ = handle_logon(&mut s, &store);
        });
        challenge_client(&"A".repeat(200))
            .write(&mut client)
            .unwrap();
        match ServerOpcodeMessage::read(&mut client).unwrap() {
            ServerOpcodeMessage::CMD_AUTH_LOGON_CHALLENGE(
                CMD_AUTH_LOGON_CHALLENGE_Server::FailUnknownAccount,
            ) => {}
            other => panic!("expected a clean FailUnknownAccount, got {other:?}"),
        }
        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn any_byte_string_from_an_unauthenticated_peer_returns_rather_than_panicking() {
        use crate::codec::property_tests::Rng;
        let mut rng = Rng::new(0x4C4F_474F_4E00);
        for case in 0..300 {
            let store = provisioned_store("TEST", "PASSWORD", false);
            let (mut client, server_end) = UnixStream::pair().unwrap();
            let server = std::thread::spawn(move || {
                let mut s = server_end;
                handle_logon(&mut s, &store)
            });

            let mut buf = Vec::new();
            if case % 2 == 1 {
                buf.push([0u8, 1, 16][rng.below(3)]);
            }
            let noise_len = rng.below(80);
            buf.extend_from_slice(&rng.bytes(noise_len));
            let _ = client.write_all(&buf);
            drop(client);

            let _resolved = server.join().unwrap_or_else(|_| {
                panic!(
                    "handle_logon panicked on unauthenticated input (case {case}): {buf:02X?} — a \
                     malformed connect must close the connection, not unwind the thread"
                )
            });
        }
    }
}
