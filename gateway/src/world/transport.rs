use super::*;
use crate::accept::BlockingTaskCapacity;
use std::sync::Mutex;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

struct Seat {
    queue: Arc<LoginQueue>,
    queued: Option<login_queue::Ticket>,
}

impl Drop for Seat {
    fn drop(&mut self) {
        match self.queued {
            Some(ticket) => self.queue.cancel_or_depart(ticket),
            None => self.queue.depart(),
        }
    }
}

async fn authenticate<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    store: Arc<dyn WorldStore>,
    actions: &BlockingTaskCapacity,
) -> Result<Option<(WorldConn, EncrypterHalf)>> {
    let seed = ProofSeed::new();
    let mut frame = Vec::new();
    SMSG_AUTH_CHALLENGE {
        server_seed: seed.seed(),
    }
    .write_unencrypted_server(&mut frame)?;
    stream.write_all(&frame).await?;

    let mut header = [0; 6];
    if let Err(error) = stream.read_exact(&mut header).await {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            return Ok(None);
        }
        return Err(error.into());
    }
    let size = u16::from_be_bytes([header[0], header[1]]) as usize;
    let body_len = size
        .checked_sub(4)
        .ok_or_else(|| anyhow!("invalid world auth frame size"))?;
    let mut frame = Vec::with_capacity(6 + body_len);
    frame.extend_from_slice(&header);
    frame.resize(6 + body_len, 0);
    stream.read_exact(&mut frame[6..]).await?;
    let claim = read_auth_session(&mut frame.as_slice())?;
    let username = claim.username.to_uppercase();
    let lookup_store = store.clone();
    let lookup_name = username.clone();
    let session = actions
        .run(move || lookup_store.lookup_session(&lookup_name))
        .await??;
    let Some(WorldSession {
        account_id,
        session_key,
    }) = session
    else {
        let mut frame = Vec::new();
        SMSG_AUTH_RESPONSE::AuthUnknownAccount.write_unencrypted_server(&mut frame)?;
        stream.write_all(&frame).await?;
        return Ok(None);
    };
    let name =
        NormalizedString::new(&username).map_err(|error| anyhow!("bad username: {error}"))?;
    let crypto = match seed.into_server_header_crypto(
        &name,
        session_key,
        claim.client_proof,
        claim.client_seed,
    ) {
        Ok(crypto) => crypto,
        Err(_) => {
            let mut frame = Vec::new();
            SMSG_AUTH_RESPONSE::AuthFailed.write_unencrypted_server(&mut frame)?;
            stream.write_all(&frame).await?;
            return Ok(None);
        }
    };
    let (encrypt, decrypt) = crypto.split();
    Ok(Some((
        WorldConn {
            protocol: ProtocolSession::new(account_id, username),
            session_claim: None,
            decrypt,
            move_coalesce: CoalesceState::default(),
            unavailable_notices: Default::default(),
            store: RoutedStore::new(store),
            session_key: Some(session_key),
            guild_signed_on: None,
            move_desync_drops: 0,
            chat_flood: Default::default(),
        },
        encrypt,
    )))
}

async fn admit<S: AsyncWrite + Unpin>(
    stream: &mut S,
    encrypt: &mut EncrypterHalf,
    queue: Arc<LoginQueue>,
) -> Result<Seat> {
    let mut seat = Seat {
        queued: None,
        queue,
    };
    if let Admission::Queued(ticket) = seat.queue.request() {
        seat.queued = Some(ticket);
        let mut last_sent: Option<(Instant, usize)> = None;
        while let Some(position) = seat.queue.position(ticket) {
            if last_sent.is_none_or(|(at, previous)| {
                previous != position || at.elapsed() >= QUEUE_RESEND_INTERVAL
            }) {
                let mut frame = Vec::new();
                (SMSG_AUTH_RESPONSE::AuthWaitQueue {
                    queue_position: position as u32,
                })
                .write_encrypted_server(&mut frame, encrypt)?;
                tokio::time::timeout(WORLD_AUTH_DEADLINE, stream.write_all(&frame)).await??;
                last_sent = Some((Instant::now(), position));
            }
            tokio::time::sleep(QUEUE_POLL_INTERVAL).await;
        }
        seat.queued = None;
    }
    let mut frame = Vec::new();
    (SMSG_AUTH_RESPONSE::AuthOk {
        billing_flags: 0,
        billing_rested: 0,
        billing_time: 0,
    })
    .write_encrypted_server(&mut frame, encrypt)?;
    tokio::time::timeout(WORLD_AUTH_DEADLINE, stream.write_all(&frame)).await??;
    Ok(seat)
}

pub(super) async fn run_at<S>(
    stream: S,
    store: Arc<dyn WorldStore>,
    queue: Arc<LoginQueue>,
    actions: BlockingTaskCapacity,
    encoders: BlockingTaskCapacity,
    deadline: tokio::time::Instant,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (stop, receiver) = tokio::sync::watch::channel(false);
    struct StopOnDrop(tokio::sync::watch::Sender<bool>);
    impl Drop for StopOnDrop {
        fn drop(&mut self) {
            self.0.send_replace(true);
        }
    }
    let _stop_on_drop = StopOnDrop(stop);
    // The supervisor owns cleanup even if the caller drops its session future.
    tokio::spawn(run_session(
        stream, store, queue, actions, encoders, receiver, deadline,
    ))
    .await?
}

async fn run_session<S>(
    mut stream: S,
    store: Arc<dyn WorldStore>,
    queue: Arc<LoginQueue>,
    actions: BlockingTaskCapacity,
    encoders: BlockingTaskCapacity,
    mut stopped: tokio::sync::watch::Receiver<bool>,
    deadline: tokio::time::Instant,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let authenticated = tokio::select! {
        biased;
        _ = stopped.wait_for(|stop| *stop) => return Ok(()),
        result = tokio::time::timeout_at(deadline, authenticate(&mut stream, store, &actions)) =>
            result.map_err(|_| anyhow!("no CMSG_AUTH_SESSION before the total pre-auth deadline"))??,
    };
    let Some((conn, mut encrypt)) = authenticated else {
        return Ok(());
    };
    let _seat = tokio::select! {
        biased;
        _ = stopped.wait_for(|stop| *stop) => return Ok(()),
        result = admit(&mut stream, &mut encrypt, queue) => result?,
    };
    let account_id = conn.account_id;
    let conn = Arc::new(Mutex::new(conn));
    let addon = Arc::new(Mutex::new(AddonLimit::default()));
    let (tx, receiver, mut closed) = egress::channel();
    let (mut reader, writer) = tokio::io::split(stream);
    let writer_close = tx.close_socket.clone();
    let mut writer_task = tokio::spawn(async move {
        let result = egress::write(writer, encrypt, receiver, encoders, account_id).await;
        if let Some(close) = writer_close {
            close();
        }
        result
    });

    let mut drain = true;
    let result = async {
        loop {
            let frame = async {
                let mut bytes = [0; 6];
                reader.read_exact(&mut bytes).await?;
                let header = conn
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .decrypt
                    .decrypt_client_header(bytes);
                let length = (header.size as usize).checked_sub(4).ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "invalid client frame size",
                    )
                })?;
                let mut body = vec![0; length];
                reader.read_exact(&mut body).await?;
                Ok::<_, std::io::Error>((header, body))
            };
            let (header, body) = tokio::select! {
                biased;
                _ = stopped.wait_for(|stop| *stop) => { drain = false; break; },
                _ = closed.wait_for(|closed| *closed) => { drain = false; break; },
                result = frame => match result {
                    Ok(frame) => frame,
                    Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
                    Err(error) => return Err(error.into()),
                }
            };
            let conn = conn.clone();
            let addon = addon.clone();
            let tx = tx.clone();
            // Await accepted game work before cleanup so a late login cannot outlive its session.
            actions
                .run(move || {
                    let mut conn = conn.lock().unwrap_or_else(|error| error.into_inner());
                    let mut addon = addon.lock().unwrap_or_else(|error| error.into_inner());
                    process_frame(&tx, &mut conn, &mut addon, header, body)
                })
                .await??;
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;

    drain &= !*closed.borrow() && !*stopped.borrow();
    if !drain {
        tx.close();
        writer_task.abort();
    }
    let cleanup = actions
        .run(move || {
            let mut conn = conn.lock().unwrap_or_else(|error| error.into_inner());
            log::debug!(
                "world movement account={account_id} received={} forwarded={}",
                conn.move_coalesce.received(),
                conn.move_coalesce.forwarded()
            );
            conn.leave_world()
        })
        .await
        .and_then(|result| result);
    if let Err(error) = cleanup {
        log::warn!("logout for account {account_id} failed: {error:#}");
    }
    drop(tx);
    let writer_result = match tokio::time::timeout(egress::MAX_AGE, &mut writer_task).await {
        Ok(result) => result,
        Err(_) => {
            writer_task.abort();
            writer_task.await
        }
    };
    result?;
    if let Ok(result) = writer_result {
        result?;
    }
    Ok(())
}

#[cfg(test)]
async fn run<S>(
    stream: S,
    store: Arc<dyn WorldStore>,
    queue: Arc<LoginQueue>,
    actions: BlockingTaskCapacity,
    encoders: BlockingTaskCapacity,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    run_at(
        stream,
        store,
        queue,
        actions,
        encoders,
        tokio::time::Instant::now() + WORLD_AUTH_DEADLINE,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn idle_world_sessions_leave_the_action_worker_available() {
        use crate::world::test_support::{
            client_handshake, tester_store, world_session_socket_pair, K,
        };
        use wow_world_messages::vanilla::{ClientMessage, CMSG_PING};

        let actions = BlockingTaskCapacity::new(1);
        let encoders = BlockingTaskCapacity::new(1);
        let queue = Arc::new(LoginQueue::new(16, 0));
        let mut sessions = Vec::new();
        let mut clients = Vec::new();
        for account in 1..=8 {
            let (mut client, server) = world_session_socket_pair();
            server.set_nonblocking(true).unwrap();
            let server = tokio::net::UnixStream::from_std(server).unwrap();
            sessions.push(tokio::spawn(run(
                server,
                Arc::new(tester_store(account)),
                queue.clone(),
                actions.clone(),
                encoders.clone(),
            )));
            clients.push(
                tokio::task::spawn_blocking(move || {
                    let (encrypt, decrypt) = client_handshake(&mut client, "TESTER", K);
                    (client, encrypt, decrypt)
                })
                .await
                .unwrap(),
            );
        }
        assert_eq!(queue.active(), 8);
        assert_eq!(actions.available(), 1);
        tokio::task::spawn_blocking(move || {
            for (mut client, mut encrypt, mut decrypt) in clients {
                CMSG_PING {
                    sequence_id: 123,
                    round_time_in_ms: 0,
                }
                .write_encrypted_client(&mut client, &mut encrypt)
                .unwrap();
                match ServerOpcodeMessage::read_encrypted(&mut client, &mut decrypt).unwrap() {
                    ServerOpcodeMessage::SMSG_PONG(pong) => assert_eq!(pong.sequence_id, 123),
                    other => panic!("expected SMSG_PONG, got {other}"),
                }
            }
        })
        .await
        .unwrap();
        for session in sessions {
            tokio::time::timeout(Duration::from_secs(5), session)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
        assert_eq!(queue.active(), 0);
    }

    #[test]
    fn cancellation_after_queue_admission_returns_the_promoted_seat() {
        let queue = Arc::new(LoginQueue::new(1, 0));
        assert_eq!(queue.request(), Admission::Admitted);
        let Admission::Queued(ticket) = queue.request() else {
            panic!("queue is full");
        };
        let seat = Seat {
            queue: queue.clone(),
            queued: Some(ticket),
        };
        queue.depart();
        assert_eq!(queue.active(), 1);
        drop(seat);
        assert_eq!(queue.active(), 0);
    }

    #[test]
    fn cancellation_before_admission_leaves_the_current_player_seated() {
        let queue = Arc::new(LoginQueue::new(1, 0));
        assert_eq!(queue.request(), Admission::Admitted);
        let Admission::Queued(ticket) = queue.request() else {
            panic!("queue is full");
        };
        drop(Seat {
            queue: queue.clone(),
            queued: Some(ticket),
        });
        assert_eq!(queue.active(), 1);
        assert_eq!(queue.depth(), 0);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn client_half_close_drains_accepted_action_replies() {
        use crate::world::test_support::{
            client_handshake, tester_store, world_session_socket_pair, K,
        };
        use wow_world_messages::vanilla::{ClientMessage, CMSG_PING};

        let (mut client, server) = world_session_socket_pair();
        server.set_nonblocking(true).unwrap();
        let server = tokio::net::UnixStream::from_std(server).unwrap();
        let queue = Arc::new(LoginQueue::new(1, 0));
        let session = tokio::spawn(run(
            server,
            Arc::new(tester_store(1)),
            queue.clone(),
            BlockingTaskCapacity::new(1),
            BlockingTaskCapacity::new(1),
        ));
        tokio::task::spawn_blocking(move || {
            let (mut encrypt, mut decrypt) = client_handshake(&mut client, "TESTER", K);
            for sequence_id in 1..=3 {
                CMSG_PING {
                    sequence_id,
                    round_time_in_ms: 0,
                }
                .write_encrypted_client(&mut client, &mut encrypt)
                .unwrap();
            }
            client.shutdown(std::net::Shutdown::Write).unwrap();
            for expected in 1..=3 {
                match ServerOpcodeMessage::read_encrypted(&mut client, &mut decrypt).unwrap() {
                    ServerOpcodeMessage::SMSG_PONG(pong) => assert_eq!(pong.sequence_id, expected),
                    other => panic!("expected SMSG_PONG, got {other}"),
                }
            }
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), session)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(queue.active(), 0);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_transfer_delivers_its_refusal_before_session_cleanup() {
        use crate::world::test_support::{
            client_handshake, drain_world_entry, quest_store, world_session_socket_pair, K,
        };
        use wow_world_messages::vanilla::{
            ClientMessage, CMSG_PLAYER_LOGIN, MSG_MOVE_WORLDPORT_ACK,
        };

        let mut fake = quest_store();
        fake.session.entity_in_world = false;
        fake.session.worldport_login_error = Some("destination unavailable".into());
        let store = Arc::new(fake);
        let (mut client, server) = world_session_socket_pair();
        server.set_nonblocking(true).unwrap();
        let session = tokio::spawn(run(
            tokio::net::UnixStream::from_std(server).unwrap(),
            store.clone(),
            Arc::new(LoginQueue::unlimited()),
            BlockingTaskCapacity::new(1),
            BlockingTaskCapacity::new(1),
        ));
        tokio::task::spawn_blocking(move || {
            let (mut encrypt, mut decrypt) = client_handshake(&mut client, "TESTER", K);
            CMSG_PLAYER_LOGIN {
                guid: wow_world_messages::Guid::new(1),
            }
            .write_encrypted_client(&mut client, &mut encrypt)
            .unwrap();
            drain_world_entry(&mut client, &mut decrypt);
            MSG_MOVE_WORLDPORT_ACK {}
                .write_encrypted_client(&mut client, &mut encrypt)
                .unwrap();
            let header = decrypt.read_and_decrypt_server_header(&mut client).unwrap();
            let mut body = vec![0; usize::from(header.size) - 2];
            std::io::Read::read_exact(&mut client, &mut body).unwrap();
            assert_eq!((header.opcode, body), (0x0040, vec![0x02]));
        })
        .await
        .unwrap();
        let error = tokio::time::timeout(Duration::from_secs(5), session)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains("destination unavailable"));
        assert!(store.session.logout_called.load(Ordering::Relaxed));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_during_login_waits_for_the_action_then_releases_the_claim() {
        use crate::world::test_support::{
            client_handshake, quest_store, world_session_socket_pair, K,
        };
        use wow_world_messages::vanilla::{ClientMessage, CMSG_PLAYER_LOGIN};

        let pause = Arc::new((std::sync::Barrier::new(2), std::sync::Barrier::new(2)));
        let mut fake = quest_store();
        fake.session.login_pause = Some(pause.clone());
        let store = Arc::new(fake);
        let queue = Arc::new(LoginQueue::new(1, 0));
        let (mut client, server) = world_session_socket_pair();
        server.set_nonblocking(true).unwrap();
        let server = tokio::net::UnixStream::from_std(server).unwrap();
        let session = tokio::spawn(run(
            server,
            store.clone(),
            queue.clone(),
            BlockingTaskCapacity::new(1),
            BlockingTaskCapacity::new(1),
        ));
        let entered = pause.clone();
        let client = tokio::task::spawn_blocking(move || {
            let (mut encrypt, _) = client_handshake(&mut client, "TESTER", K);
            CMSG_PLAYER_LOGIN {
                guid: wow_world_messages::Guid::new(1),
            }
            .write_encrypted_client(&mut client, &mut encrypt)
            .unwrap();
            entered.0.wait();
            client
        })
        .await
        .unwrap();
        session.abort();
        assert!(session.await.unwrap_err().is_cancelled());
        assert_eq!(queue.active(), 1);
        assert!(!store.session.logout_called.load(Ordering::Relaxed));
        tokio::task::spawn_blocking(move || pause.1.wait())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while queue.active() != 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(store.session.logout_called.load(Ordering::Relaxed));
        drop(client);
    }
}
