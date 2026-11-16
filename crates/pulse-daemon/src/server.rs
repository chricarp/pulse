//! The accept loop for the one HTTP listener. `axum::serve` waits for every connection on a
//! graceful shutdown, with no bound, and reads request headers with no timeout. Anything that
//! can reach 127.0.0.1 can open a connection without a token, so that let any local process
//! (another user's included) hold shutdown, and `daemon.lock` with it, forever by sending half
//! a request. Here:
//!
//! - **Headers** must arrive within [`Limits::header_read`], also between requests on a
//!   keep-alive connection; then the connection is closed.
//! - **At most [`Limits::max_connections`]** HTTP connections are served at once. Further ones
//!   wait in the kernel's accept queue, so a flood cannot use up the daemon's file descriptors
//!   (which SQLite, sources and run pipes need). An upgraded WebSocket no longer counts: it
//!   carries the daemon token, and its end is the RPC transport's.
//! - **Shutdown** stops accepting, asks each connection to finish its current response, and
//!   after [`Limits::drain`] drops whatever is left.
//!
//! **Not bounded here:** a request body sent slowly to a handler that reads it without a token
//! (`/auth/login`) holds its slot until it ends, and a local process can hold every slot by
//! keeping requests going. That denies the listener, not the rest of the daemon; nothing on a
//! loopback TCP socket says which local user is calling, so there is nothing cheap to ration by.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub header_read: Duration,
    pub drain: Duration,
    pub max_connections: usize,
}

/// What the daemon uses. 512 connections leave room under the common soft limit of 1024 open
/// files; 30 s for headers is hyper's own default, which only takes effect with a timer.
pub(crate) const LIMITS: Limits = Limits {
    header_read: Duration::from_secs(30),
    drain: Duration::from_secs(3),
    max_connections: 512,
};

/// Serves `app` until `stop` is cancelled, then drains for at most `limits.drain`.
pub(crate) async fn serve(
    listener: TcpListener,
    app: Router,
    stop: CancellationToken,
    limits: Limits,
) {
    let slots = Arc::new(Semaphore::new(limits.max_connections));
    let mut connections = JoinSet::new();
    loop {
        let slot = tokio::select! {
            () = stop.cancelled() => break,
            Some(_) = connections.join_next() => continue,
            slot = slots.clone().acquire_owned() => match slot {
                Ok(slot) => slot,
                // Never closed.
                Err(_) => break,
            },
        };
        let stream = tokio::select! {
            () = stop.cancelled() => break,
            Some(_) = connections.join_next() => continue,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(e) => {
                    // A connection reset before it was accepted is not worth a pause; anything
                    // else (out of file descriptors) would repeat at once.
                    if !matches!(
                        e.kind(),
                        std::io::ErrorKind::ConnectionAborted | std::io::ErrorKind::ConnectionReset
                    ) {
                        tracing::warn!("cannot accept a connection: {e}");
                        tokio::select! {
                            () = stop.cancelled() => break,
                            () = tokio::time::sleep(Duration::from_secs(1)) => {}
                        }
                    }
                    continue;
                }
            },
        };
        let service = TowerToHyperService::new(app.clone());
        let stop = stop.clone();
        connections.spawn(async move {
            let _slot = slot;
            let mut builder = hyper::server::conn::http1::Builder::new();
            builder.timer(TokioTimer::new()).header_read_timeout(limits.header_read);
            let conn = builder.serve_connection(TokioIo::new(stream), service).with_upgrades();
            tokio::pin!(conn);
            tokio::select! {
                _ = conn.as_mut() => return,
                () = stop.cancelled() => conn.as_mut().graceful_shutdown(),
            }
            let _ = conn.await;
        });
    }
    drop(listener);
    let drained = tokio::time::timeout(limits.drain, async {
        while connections.join_next().await.is_some() {}
    })
    .await;
    if drained.is_err() {
        tracing::warn!(
            "{} connections were still open {:?} after shutdown began; closing them",
            connections.len(),
            limits.drain
        );
        connections.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::routing::get;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tokio::sync::Notify;
    use tokio::time::Instant;

    use super::*;

    /// Far beyond every limit under test. Time is paused, so reaching it costs nothing and
    /// means the server never let go.
    const NEVER: Duration = Duration::from_secs(7 * 24 * 3600);

    const TEST_LIMITS: Limits = Limits {
        header_read: Duration::from_secs(30),
        drain: Duration::from_secs(3),
        max_connections: 2,
    };

    /// A server whose `/hang` handler signals `entered` and never answers.
    async fn start(
        limits: Limits,
    ) -> (std::net::SocketAddr, CancellationToken, Arc<Notify>, tokio::task::JoinHandle<()>) {
        let entered = Arc::new(Notify::new());
        let signal = entered.clone();
        let app = Router::new().route("/", get(|| async { "ok" })).route(
            "/hang",
            get(move || {
                let signal = signal.clone();
                async move {
                    signal.notify_one();
                    std::future::pending::<&'static str>().await
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = CancellationToken::new();
        let server = tokio::spawn(serve(listener, app, stop.clone(), limits));
        (addr, stop, entered, server)
    }

    /// Sends a whole keep-alive request for `/` and reads its response.
    async fn get_root(conn: &mut TcpStream) {
        conn.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").await.unwrap();
        let mut got = Vec::new();
        let mut buf = [0u8; 512];
        while !got.ends_with(b"ok") {
            let n = conn.read(&mut buf).await.unwrap();
            assert!(n > 0, "closed before the response: {:?}", String::from_utf8_lossy(&got));
            got.extend_from_slice(&buf[..n]);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_does_not_wait_for_a_connection_that_never_finishes() {
        let (addr, stop, entered, server) = start(TEST_LIMITS).await;
        let mut conn = TcpStream::connect(addr).await.unwrap();
        conn.write_all(b"GET /hang HTTP/1.1\r\nHost: x\r\n\r\n").await.unwrap();
        entered.notified().await;
        let start = Instant::now();
        stop.cancel();
        tokio::time::timeout(NEVER, server).await.expect("shutdown is bounded").unwrap();
        assert_eq!(start.elapsed(), TEST_LIMITS.drain);
    }

    #[tokio::test(start_paused = true)]
    async fn a_half_sent_request_is_dropped_after_the_header_timeout() {
        let (addr, _stop, _, _server) = start(TEST_LIMITS).await;
        let mut conn = TcpStream::connect(addr).await.unwrap();
        let start = Instant::now();
        conn.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n").await.unwrap();
        let mut buf = [0u8; 64];
        let read = tokio::time::timeout(NEVER, conn.read(&mut buf)).await;
        assert!(matches!(read, Ok(Ok(0)) | Ok(Err(_))), "the server closes it: {read:?}");
        assert!(start.elapsed() >= TEST_LIMITS.header_read);
    }

    #[tokio::test(start_paused = true)]
    async fn connections_beyond_the_limit_wait_for_a_free_slot() {
        let (addr, _stop, _, _server) = start(TEST_LIMITS).await;
        let start = Instant::now();
        // Two idle keep-alive connections hold both slots until their header timeout.
        let mut held = Vec::new();
        for _ in 0..TEST_LIMITS.max_connections {
            let mut conn = TcpStream::connect(addr).await.unwrap();
            get_root(&mut conn).await;
            held.push(conn);
        }
        assert!(start.elapsed() < TEST_LIMITS.header_read);
        let mut third = TcpStream::connect(addr).await.unwrap();
        tokio::time::timeout(NEVER, get_root(&mut third)).await.expect("served once a slot frees");
        assert!(start.elapsed() >= TEST_LIMITS.header_read, "not before a slot was free");
    }
}
