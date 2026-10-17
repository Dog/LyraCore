use super::{Outbound, SessionSender, SessionTx, WriterTrace};
use crate::accept::BlockingTaskCapacity;
use anyhow::{anyhow, bail, Result};
use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc::SendError, Arc};
use std::time::{Duration, Instant};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, watch};
use wow_srp::vanilla_header::EncrypterHalf;

const QUEUE_ITEMS: usize = 2048;
const QUEUE_BYTES: usize = 4 * 1024 * 1024;
const TYPED_RESERVATION: usize = 4 * 1024;
// A queued Relay Job holds shared rows; its encoded size is checked when it runs.
const JOB_RESERVATION: usize = 256;
pub(super) const MAX_AGE: Duration = Duration::from_secs(10);
const WRITE_BYTES: usize = 64 * 1024;
const ENCODE_BATCH: usize = 32;

pub(crate) static ENCODED_ENTRIES: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
pub(super) struct QueueSender {
    sender: mpsc::Sender<Queued>,
    bytes: Arc<AtomicUsize>,
    depth: Arc<AtomicUsize>,
    closed: watch::Sender<bool>,
}

pub(super) struct Queued {
    out: Option<Outbound>,
    since: Instant,
    reservation: usize,
    bytes: Arc<AtomicUsize>,
    depth: Arc<AtomicUsize>,
}

impl Drop for Queued {
    fn drop(&mut self) {
        self.bytes.fetch_sub(self.reservation, Ordering::Relaxed);
        self.depth.fetch_sub(1, Ordering::Relaxed);
    }
}

impl QueueSender {
    fn close(&self, reason: &str) {
        if !self.closed.send_replace(true) {
            log::warn!(
                "World Session outbound queue closed: reason={reason} queued_items={} reserved_bytes={}",
                self.depth.load(Ordering::Relaxed),
                self.bytes.load(Ordering::Relaxed),
            );
        }
    }

    pub(super) fn send(&self, out: Outbound) -> std::result::Result<(), SendError<Outbound>> {
        let reservation = match queued_bytes(&out) {
            Ok(bytes) => bytes.max(1),
            Err(_) => {
                self.close("reservation_overflow");
                return Err(SendError(out));
            }
        };
        if *self.closed.borrow() {
            return Err(SendError(out));
        }
        if self
            .bytes
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                used.checked_add(reservation)
                    .filter(|next| *next <= QUEUE_BYTES)
            })
            .is_err()
        {
            self.close("byte_capacity");
            return Err(SendError(out));
        }
        self.depth.fetch_add(1, Ordering::Relaxed);
        let queued = Queued {
            out: Some(out),
            since: Instant::now(),
            reservation,
            bytes: self.bytes.clone(),
            depth: self.depth.clone(),
        };
        self.sender.try_send(queued).map_err(|error| {
            self.close(match &error {
                mpsc::error::TrySendError::Full(_) => "item_capacity",
                mpsc::error::TrySendError::Closed(_) => "receiver_closed",
            });
            let mut queued = error.into_inner();
            SendError(
                queued
                    .out
                    .take()
                    .unwrap_or_else(|| Outbound::Batch(Vec::new())),
            )
        })
    }
}

pub(super) fn channel() -> (SessionTx, mpsc::Receiver<Queued>, watch::Receiver<bool>) {
    let (sender, receiver) = mpsc::channel(QUEUE_ITEMS);
    let (closed, close_rx) = watch::channel(false);
    let depth = Arc::new(AtomicUsize::new(0));
    let queue = QueueSender {
        sender,
        bytes: Arc::new(AtomicUsize::new(0)),
        depth: depth.clone(),
        closed: closed.clone(),
    };
    let tx = SessionTx {
        tx: SessionSender::Async(queue),
        depth,
        close_socket: Some(Arc::new(move || {
            closed.send_replace(true);
        })),
    };
    (tx, receiver, close_rx)
}

fn queued_bytes(out: &Outbound) -> io::Result<usize> {
    match out {
        Outbound::Raw { body, .. } => Ok(4 + body.len()),
        Outbound::Shared { body, .. } => Ok(4 + body.len()),
        Outbound::Batch(messages) => messages
            .len()
            .checked_mul(TYPED_RESERVATION)
            .ok_or_else(|| io::Error::other("outbound reservation overflow")),
        // Typed compression and deferred projections run only on encoding workers.
        Outbound::One(_) => Ok(TYPED_RESERVATION),
        Outbound::Job(_) => Ok(JOB_RESERVATION),
    }
}

#[derive(Clone)]
pub(crate) struct Packet {
    opcode: u16,
    body: Arc<[u8]>,
}

impl Packet {
    pub(crate) fn outbound(&self) -> Outbound {
        Outbound::Shared {
            opcode: self.opcode,
            body: self.body.clone(),
        }
    }
}

pub(crate) fn encode(outbound: Vec<Outbound>) -> io::Result<Vec<Packet>> {
    let mut packets = Vec::new();
    let mut bytes = 0usize;
    for out in outbound {
        let mut add = |opcode, body: Arc<[u8]>| -> io::Result<()> {
            if body.len() > u16::MAX as usize - 2 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "outbound body exceeds frame size",
                ));
            }
            bytes = bytes.saturating_add(4 + body.len());
            if bytes > QUEUE_BYTES {
                return Err(io::Error::other(
                    "outbound projection exceeds World Session byte limit",
                ));
            }
            packets.push(Packet { opcode, body });
            Ok(())
        };
        let mut typed =
            |message: wow_world_messages::vanilla::opcodes::ServerOpcodeMessage| -> io::Result<()> {
                let mut frame = Vec::new();
                message.write_unencrypted_server(&mut frame)?;
                if frame.len() < 4 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "short outbound frame",
                    ));
                }
                let opcode = u16::from_le_bytes([frame[2], frame[3]]);
                add(opcode, Arc::from(&frame[4..]))
            };
        match out {
            Outbound::One(message) => typed(message)?,
            Outbound::Batch(messages) => {
                for message in messages {
                    typed(message)?;
                }
            }
            Outbound::Raw { opcode, body } => {
                for violation in super::packet_lint::lint_raw(opcode, &body) {
                    log::error!("packet-lint VIOLATION opcode=0x{opcode:04X}: {violation}");
                }
                add(opcode, body.into())?;
            }
            Outbound::Shared { opcode, body } => add(opcode, body)?,
            Outbound::Job(_) => return Err(io::Error::other("nested Relay job")),
        }
    }
    Ok(packets)
}

fn is_job(queued: &Queued) -> bool {
    matches!(queued.out, Some(Outbound::Job(_)))
}

pub(super) async fn write<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut encrypt: EncrypterHalf,
    mut receiver: mpsc::Receiver<Queued>,
    closed: watch::Receiver<bool>,
    workers: BlockingTaskCapacity,
    account_id: u64,
) -> Result<()> {
    let mut trace = crate::config::writer_trace_enabled().then(WriterTrace::new);
    let mut buffer = Vec::with_capacity(4096);
    let mut pending = VecDeque::new();
    loop {
        let first = match pending.pop_front() {
            Some(first) => first,
            None => match receiver.recv().await {
                Some(first) => first,
                None => break,
            },
        };
        let mut oldest = first.since;
        let mut jobs_only = is_job(&first);
        let mut batch = vec![first];
        while batch.len() < ENCODE_BATCH {
            let Some(next) = pending.pop_front().or_else(|| receiver.try_recv().ok()) else {
                break;
            };
            // A Relay Job can close the socket; write preceding plain packets before invoking it.
            if is_job(&next) && !jobs_only {
                pending.push_front(next);
                break;
            }
            jobs_only &= is_job(&next);
            oldest = oldest.min(next.since);
            batch.push(next);
        }
        ENCODED_ENTRIES.fetch_add(batch.len() as u64, Ordering::Relaxed);
        let deadline = tokio::time::Instant::from_std(oldest + MAX_AGE);
        if tokio::time::Instant::now() >= deadline {
            bail!("World Session outbound queue exceeded age limit");
        }
        crate::stage_timing::QUEUE_WAIT.record(oldest.elapsed());
        let job_closed = closed.clone();
        let submitted = Instant::now();
        let encoded = tokio::time::timeout_at(
            deadline,
            workers.run(move || {
                let encoding = Instant::now();
                crate::stage_timing::ENCODER_WAIT.record(encoding - submitted);
                let mut frames = Vec::new();
                let mut total = 0usize;
                for queued in &mut batch {
                    if *job_closed.borrow() || total >= QUEUE_BYTES {
                        break;
                    }
                    let outbound = match queued.out.take() {
                        Some(Outbound::Job(job)) => job(),
                        Some(other) => vec![other],
                        None => bail!("World Session queue entry consumed twice"),
                    };
                    let packets = encode(outbound)?;
                    total += packets
                        .iter()
                        .map(|packet| 4 + packet.body.len())
                        .sum::<usize>();
                    frames.extend(packets);
                }
                crate::stage_timing::ENCODE.record(encoding.elapsed());
                Ok::<_, anyhow::Error>((batch, frames))
            }),
        )
        .await
        .map_err(|_| anyhow!("World Session outbound queue exceeded age limit"))???;
        let (batch, frames) = encoded;
        if *closed.borrow() {
            return Ok(());
        }
        let (unencoded, _reservations): (Vec<_>, Vec<_>) =
            batch.into_iter().partition(|queued| queued.out.is_some());
        for queued in unencoded.into_iter().rev() {
            pending.push_front(queued);
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("World Session outbound encoding exceeded age limit");
        }
        let wrote = !frames.is_empty();
        buffer.clear();
        let result = async {
            for frame in frames {
                if !buffer.is_empty() && buffer.len() + 4 + frame.body.len() > WRITE_BYTES {
                    writer.write_all(&buffer).await?;
                    buffer.clear();
                }
                if let Some(trace) = trace.as_mut() {
                    trace.push(
                        frame.opcode,
                        (2 + frame.body.len()) as u16,
                        super::fnv1a64(&frame.body),
                    );
                }
                encrypt.write_encrypted_server_header(
                    &mut buffer,
                    (2 + frame.body.len()) as u16,
                    frame.opcode,
                )?;
                buffer.extend_from_slice(&frame.body);
            }
            if !buffer.is_empty() {
                writer.write_all(&buffer).await?;
            }
            Ok::<_, io::Error>(())
        };
        let writing = Instant::now();
        let result = tokio::time::timeout_at(deadline, result).await;
        if wrote {
            crate::stage_timing::SOCKET_WRITE.record(writing.elapsed());
        }
        let result = result
            .map_err(|_| anyhow!("World Session outbound write exceeded age limit"))
            .and_then(|result| result.map_err(Into::into));
        if let Err(error) = result {
            if let Some(trace) = trace.as_ref() {
                trace.dump(account_id, &error.to_string());
            }
            return Err(error);
        }
    }
    writer.shutdown().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deferred_jobs_cannot_grow_the_queue_without_bound() {
        let (tx, _receiver, closed) = channel();
        for _ in 0..QUEUE_ITEMS {
            tx.send(Outbound::Job(Box::new(Vec::new)))
                .unwrap_or_else(|_| panic!("within budget"));
        }
        assert!(tx.send(Outbound::Job(Box::new(Vec::new))).is_err());
        assert!(*closed.borrow());
    }

    #[tokio::test]
    async fn dropping_a_queued_packet_returns_its_budget() {
        let (tx, mut receiver, _) = channel();
        for _ in 0..100 {
            tx.send(Outbound::Raw {
                opcode: 1,
                body: vec![0; 60_000],
            })
            .unwrap_or_else(|_| panic!("drained queue must retain its budget"));
            assert_eq!(tx.depth(), 1);
            drop(receiver.recv().await);
            assert_eq!(tx.depth(), 0);
        }
    }

    #[test]
    fn oversized_shared_packets_are_refused_before_encryption() {
        assert!(encode(vec![Outbound::Shared {
            opcode: 1,
            body: vec![0; u16::MAX as usize].into()
        }])
        .is_err());
    }

    #[tokio::test]
    async fn an_expired_entry_later_in_the_batch_refuses_the_write() {
        let (tx, mut queued, _) = channel();
        for _ in 0..2 {
            tx.send(Outbound::Raw {
                opcode: 0x01dd,
                body: vec![0; 4],
            })
            .unwrap_or_else(|_| panic!("within budget"));
        }
        let (sender, receiver) = mpsc::channel(2);
        sender
            .send(queued.recv().await.unwrap())
            .await
            .ok()
            .unwrap();
        let mut delayed = queued.recv().await.unwrap();
        delayed.since -= MAX_AGE + Duration::from_secs(1);
        sender.send(delayed).await.ok().unwrap();
        drop(sender);
        let (_, crypto) = wow_srp::vanilla_header::ProofSeed::new().into_client_header_crypto(
            &wow_srp::normalized_string::NormalizedString::new("TESTER").unwrap(),
            super::super::test_support::K,
            0,
        );

        let error = write(
            tokio::io::sink(),
            crypto.split().0,
            receiver,
            watch::channel(false).1,
            BlockingTaskCapacity::new(1),
            1,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("age limit"));
        assert_eq!(tx.depth(), 0);
    }

    #[tokio::test]
    async fn jobs_past_the_batch_byte_limit_are_written_in_a_later_batch() {
        let (tx, receiver, closed) = channel();
        let ran = Arc::new(AtomicUsize::new(0));
        for _ in 0..3 {
            let ran = ran.clone();
            tx.send(Outbound::Job(Box::new(move || {
                ran.fetch_add(1, Ordering::Relaxed);
                (0..40)
                    .map(|_| Outbound::Raw {
                        opcode: 0x01dd,
                        body: vec![0; 60_000],
                    })
                    .collect()
            })))
            .unwrap_or_else(|_| panic!("within budget"));
        }
        drop(tx);
        let (_, crypto) = wow_srp::vanilla_header::ProofSeed::new().into_client_header_crypto(
            &wow_srp::normalized_string::NormalizedString::new("TESTER").unwrap(),
            super::super::test_support::K,
            0,
        );

        write(
            tokio::io::sink(),
            crypto.split().0,
            receiver,
            closed,
            BlockingTaskCapacity::new(1),
            1,
        )
        .await
        .unwrap();
        assert_eq!(ran.load(Ordering::Relaxed), 3);
    }

    #[tokio::test]
    async fn jobs_queued_behind_a_closing_job_do_not_run() {
        let (tx, receiver, closed) = channel();
        let ran = Arc::new(AtomicUsize::new(0));
        let closing = tx.clone();
        tx.send(Outbound::Job(Box::new(move || {
            closing.close();
            Vec::new()
        })))
        .unwrap_or_else(|_| panic!("within budget"));
        for _ in 0..3 {
            let ran = ran.clone();
            tx.send(Outbound::Job(Box::new(move || {
                ran.fetch_add(1, Ordering::Relaxed);
                Vec::new()
            })))
            .unwrap_or_else(|_| panic!("within budget"));
        }
        drop(tx);
        let (_, crypto) = wow_srp::vanilla_header::ProofSeed::new().into_client_header_crypto(
            &wow_srp::normalized_string::NormalizedString::new("TESTER").unwrap(),
            super::super::test_support::K,
            0,
        );

        write(
            tokio::io::sink(),
            crypto.split().0,
            receiver,
            closed,
            BlockingTaskCapacity::new(1),
            1,
        )
        .await
        .unwrap();
        assert_eq!(ran.load(Ordering::Relaxed), 0);
    }
}
