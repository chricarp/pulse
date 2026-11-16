//! How clients authenticate to the daemon's one local listener. The constants here are also
//! exported to TypeScript (`apps/web/src/contracts/index.ts`), so the web UI, the desktop shell
//! and the daemon cannot drift apart.
//!
//! ## The daemon proves itself first
//!
//! The port is fixed and anyone on the machine can listen on it while the daemon is stopped. So
//! no client sends a credential to the port until whatever answers there has shown that it
//! knows that credential. Once per connection attempt the client picks a fresh random `nonce`
//! and asks `GET` [`PROOF_PATH`]`?nonce=<nonce>` ([`ProofQuery`]); the daemon answers
//! [`DaemonProof`] `{ instance, proof }` with
//!
//! ```text
//! key   = HMAC-SHA256(key = credential, message = PROOF_KEY_LABEL)
//! proof = hex(HMAC-SHA256(key, message = port + "\0" + instance + "\0" + nonce))
//! ```
//!
//! where `credential` is the UTF-8 text of what the client is about to send: the daemon token;
//! with `agent=<name>`, that agent's MCP token; with `resume=true`, this daemon instance's UI
//! resume code. `port` is the daemon's port in decimal; the client compares against the port it
//! connected to, so a program on another port cannot relay a real daemon's answer. `instance`
//! is random per daemon start. The client compares in constant time and, if the proof is wrong
//! or missing, does not send the credential and says that something else holds Pulse's port.
//!
//! The answer is a MAC under a key used for nothing else, over a message that always starts with
//! the port, so asking for proofs (which needs no credential) yields nothing that verifies as a
//! token, a login code or a resume code. The Rust clients send the proof request and the
//! credential on one keep-alive connection; browsers and the desktop shell cannot, so for them a
//! daemon that exits in the milliseconds between the two requests, with another program ready
//! to take the port, would still receive the token (see the threat model).
//!
//! ## Credentials
//!
//! **Processes that can read `$PULSE_HOME/token`** (the CLI, the desktop shell, tests) send the
//! daemon token as `Authorization: Bearer <token>` on `/ws` and `/ingest` (the shell as a
//! WebSocket subprotocol, like a browser). A wrong token is refused with HTTP 401 before any
//! upgrade.
//!
//! **The browser** cannot read the token file and cannot set headers on a WebSocket:
//!
//! 1. `pulse open` (or the desktop shell) mints a **login code** with the token and opens
//!    `http://127.0.0.1:<port>/#login=<code>`. `pulse open` does that through a mode-0600 HTML
//!    file in the Pulse home that redirects there, so the code never appears in a process's
//!    argument list. The code is single-use and valid for two minutes (and only for the daemon
//!    that was running when it was minted: a restarted daemon refuses older codes, since it
//!    cannot know which were spent). The fragment is never sent to a server.
//! 2. The page removes the fragment from the address bar (`history.replaceState`), then posts
//!    [`UiLoginRequest`] to [`LOGIN_PATH`]. The answer is [`UiLoginResponse`] (200) or a 401 for
//!    a code that is invalid, expired or already used.
//! 3. The page keeps the token in memory only. To survive a reload of the tab it computes a
//!    **resume code** for the daemon instance it is connected to,
//!    `RESUME_CODE_PREFIX + hex(HMAC-SHA256(HMAC-SHA256(token, RESUME_KEY_LABEL), instance))`,
//!    writes it to `sessionStorage` (key [`RESUME_STORAGE_KEY`]) only when the page is being
//!    unloaded, and the next page load reads and deletes it, checks the daemon's proof for it
//!    (`resume=true`, with the same `instance`), and posts it to [`LOGIN_PATH`] as the `code`. A
//!    resume code works only on the daemon instance it was made for: a program that took the
//!    port after the daemon stopped, and reads it from storage, holds a code no daemon accepts.
//! 4. Before each WebSocket attempt it checks the daemon's proof for the token, then opens `/ws`
//!    offering the subprotocols `[WS_PROTOCOL, WS_TOKEN_PROTOCOL_PREFIX + token]`. The daemon
//!    selects [`WS_PROTOCOL`] and never echoes the token.
//! 5. A browser cannot see the HTTP status of a refused upgrade: a 401 looks exactly like a daemon
//!    that is not running (close code 1006). So an upgrade that offers [`WS_PROTOCOL`] with a
//!    missing or wrong token is accepted and immediately closed with [`WS_CLOSE_UNAUTHORIZED`]
//!    before any request is read. The page then forgets its token and asks for `pulse open`
//!    instead of retrying. Any other close means "reconnect with backoff".
//!
//! `Host` and `Origin` are checked on every request first (see the daemon's transport module);
//! the login exchange and the proof need no token but are subject to the same checks.

pub use crate::status::{DaemonProof, ProofQuery, UiLoginRequest, UiLoginResponse};

/// The URL fragment parameter that carries a login code: `#login=<code>`.
pub const LOGIN_FRAGMENT_PARAM: &str = "login";
/// `POST` a [`UiLoginRequest`] here for a [`UiLoginResponse`].
pub const LOGIN_PATH: &str = "/auth/login";
/// How long a login code stays valid, in seconds.
pub const LOGIN_CODE_TTL_SECS: u64 = 120;
/// `GET` with a [`ProofQuery`] for a [`DaemonProof`].
pub const PROOF_PATH: &str = "/auth/proof";
/// The fewest hex digits a proof nonce may have (128 bits).
pub const PROOF_NONCE_MIN_HEX: u64 = 32;
/// The most hex digits a proof nonce may have.
pub const PROOF_NONCE_MAX_HEX: u64 = 128;
/// Derives the proof key from a credential (see the module docs).
pub const PROOF_KEY_LABEL: &str = "pulse/daemon-proof/v1";
/// Derives the resume-code key from the daemon token.
pub const RESUME_KEY_LABEL: &str = "pulse/ui-resume/v1";
/// Resume codes start with this; login codes start with a digit.
pub const RESUME_CODE_PREFIX: &str = "r.";
/// Where the web UI keeps a resume code between unloading a page and loading the next
/// (`sessionStorage`, per tab). Nothing else is stored.
pub const RESUME_STORAGE_KEY: &str = "pulse.resume";
/// The WebSocket path for JSON-RPC.
pub const WS_PATH: &str = "/ws";
/// The subprotocol the daemon selects. Offered by browsers alongside the token protocol.
pub const WS_PROTOCOL: &str = "pulse";
/// A browser offers the token as the subprotocol `pulse.token.<token>`.
pub const WS_TOKEN_PROTOCOL_PREFIX: &str = "pulse.token.";
/// The close code for a WebSocket whose token is missing or wrong. Retrying will not help.
pub const WS_CLOSE_UNAUTHORIZED: u16 = 4401;

/// The constants above as TypeScript, for the generated bindings.
pub(crate) fn typescript_constants() -> String {
    let s = |name: &str, v: &str| format!("export const {name} = {v:?};\n");
    let n = |name: &str, v: u64| format!("export const {name} = {v};\n");
    [
        "/** How clients authenticate; see `pulse_contracts::auth`. */\n".to_owned(),
        s("LOGIN_FRAGMENT_PARAM", LOGIN_FRAGMENT_PARAM),
        s("LOGIN_PATH", LOGIN_PATH),
        n("LOGIN_CODE_TTL_SECS", LOGIN_CODE_TTL_SECS),
        s("PROOF_PATH", PROOF_PATH),
        n("PROOF_NONCE_MIN_HEX", PROOF_NONCE_MIN_HEX),
        n("PROOF_NONCE_MAX_HEX", PROOF_NONCE_MAX_HEX),
        s("PROOF_KEY_LABEL", PROOF_KEY_LABEL),
        s("RESUME_KEY_LABEL", RESUME_KEY_LABEL),
        s("RESUME_CODE_PREFIX", RESUME_CODE_PREFIX),
        s("RESUME_STORAGE_KEY", RESUME_STORAGE_KEY),
        s("WS_PATH", WS_PATH),
        s("WS_PROTOCOL", WS_PROTOCOL),
        s("WS_TOKEN_PROTOCOL_PREFIX", WS_TOKEN_PROTOCOL_PREFIX),
        n("WS_CLOSE_UNAUTHORIZED", u64::from(WS_CLOSE_UNAUTHORIZED)),
    ]
    .concat()
}

/// Whether `nonce` is acceptable for [`ProofQuery`]: lowercase hex of a length in bounds.
pub fn valid_proof_nonce(nonce: &str) -> bool {
    (PROOF_NONCE_MIN_HEX as usize..=PROOF_NONCE_MAX_HEX as usize).contains(&nonce.len())
        && nonce.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
