use super::super::*;
use crate::accept::BlockingTaskCapacity;
use std::io;
use tokio::io::{AsyncRead, AsyncWrite};

pub(crate) trait TestSocket: Sized {
    type Async: AsyncRead + AsyncWrite + Unpin + Send + 'static;
    fn into_async(self) -> io::Result<Self::Async>;
}

impl TestSocket for std::net::TcpStream {
    type Async = tokio::net::TcpStream;

    fn into_async(self) -> io::Result<Self::Async> {
        self.set_nonblocking(true)?;
        tokio::net::TcpStream::from_std(self)
    }
}

impl TestSocket for std::os::unix::net::UnixStream {
    type Async = tokio::net::UnixStream;

    fn into_async(self) -> io::Result<Self::Async> {
        self.set_nonblocking(true)?;
        tokio::net::UnixStream::from_std(self)
    }
}

pub(crate) fn run_world_session<S: TestSocket>(
    stream: S,
    store: Arc<dyn WorldStore>,
) -> Result<()> {
    run_world_session_with_queue(stream, store, Arc::new(LoginQueue::unlimited()))
}

pub(crate) fn run_world_session_with_queue<S: TestSocket>(
    stream: S,
    store: Arc<dyn WorldStore>,
    queue: Arc<LoginQueue>,
) -> Result<()> {
    run_world_session_with_deadline(stream, store, queue, Instant::now() + WORLD_AUTH_DEADLINE)
}

pub(crate) fn run_world_session_with_deadline<S: TestSocket>(
    stream: S,
    store: Arc<dyn WorldStore>,
    queue: Arc<LoginQueue>,
    deadline: Instant,
) -> Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(4)
        .build()?
        .block_on(async {
            let permit = Arc::new(tokio::sync::Semaphore::new(1)).try_acquire_owned()?;
            super::super::transport::run_at(
                stream.into_async()?,
                store,
                queue,
                BlockingTaskCapacity::new(2),
                BlockingTaskCapacity::new(2),
                permit,
                tokio::time::Instant::from_std(deadline),
            )
            .await
        })
}

pub(crate) fn start_writer(
    stream: std::os::unix::net::UnixStream,
    encrypt: EncrypterHalf,
) -> (SessionTx, std::thread::JoinHandle<Result<()>>) {
    let (tx, receiver, closed) = egress::channel();
    let writer = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()?
            .block_on(async {
                egress::write(
                    stream.into_async()?,
                    encrypt,
                    receiver,
                    closed,
                    BlockingTaskCapacity::new(1),
                    1,
                )
                .await
            })
    });
    (tx, writer)
}
