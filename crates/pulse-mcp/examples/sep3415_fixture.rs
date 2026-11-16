//! A throwaway MCP server for running the SEP-3415 conformance suite against this crate:
//! the real `/mcp` router over the tests' fake backend, emitting one `fs.file.created` every
//! `interval` seconds (0: never) so delivery checks have something to deliver. Run the push
//! scenario a second time with `0`: heartbeats only appear on a quiet stream.
//!
//! ```sh
//! cargo run -p pulse-mcp --features sep3415 --example sep3415_fixture -- <port> <interval>
//! npx tsx src/index.ts server --url http://127.0.0.1:<port>/mcp --scenario events-poll
//! ```
//!
//! The suite cannot send a bearer token, so this fixture adds alice's to every request. Never
//! do that outside a fixture.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

#[path = "../tests/support/mod.rs"]
mod support;

use axum::extract::Request;
use axum::middleware::Next;
use http::header;
use serde_json::json;
use support::{ALICE_TOKEN, Fake};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let port: u16 = std::env::args().nth(1).and_then(|p| p.parse().ok()).unwrap_or(0);
    let interval: u64 = std::env::args().nth(2).and_then(|p| p.parse().ok()).unwrap_or(1);
    let fake = Fake::new();
    fake.sync_clock();
    let config = fake.config();
    let app = axum::Router::new().nest("/mcp", pulse_mcp::router(fake.clone(), config)).layer(
        axum::middleware::from_fn(|mut req: Request, next: Next| async move {
            let value = format!("Bearer {ALICE_TOKEN}").parse().unwrap();
            req.headers_mut().insert(header::AUTHORIZATION, value);
            next.run(req).await
        }),
    );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    println!("http://{}/mcp", listener.local_addr().unwrap());
    tokio::spawn(async move {
        if interval == 0 {
            return;
        }
        let mut n = 0u64;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
            fake.sync_clock();
            n += 1;
            let path = format!("/home/me/University/fixture-{n}.pdf");
            fake.emit("fs.file.created", Some(&path), json!({ "path": path }));
        }
    });
    axum::serve(listener, app).await.unwrap();
}
