//! How clients authenticate to the daemon's one local listener. The constants here are also
//! exported to TypeScript (`apps/web/src/contracts/index.ts`), so the web UI and the daemon
//! cannot drift apart.
//!
//! **Processes that can read `$PULSE_HOME/token`** (the CLI, tests) send the daemon token as
//! `Authorization: Bearer <token>` on `/ws` and `/ingest`. A wrong token is refused with
//! HTTP 401 before any upgrade.
//!
//! **The browser** cannot read the token file and cannot set headers on a WebSocket:
//!
//! 1. `pulse open` (or the desktop shell) mints a **login code** with the token and opens
//!    `http://127.0.0.1:<port>/#login=<code>`. The code is single-use and valid for two minutes
//!    (and only for the daemon that was running when it was minted: a restarted daemon refuses
//!    older codes, since it cannot know which were spent);
//!    the token itself never appears in a process argument list or in browser history. The
//!    fragment is never sent to a server.
//! 2. The page removes the fragment from the address bar (`history.replaceState`), then posts
//!    [`UiLoginRequest`] to [`LOGIN_PATH`]. The answer is [`UiLoginResponse`] (200) or a 401 for
//!    a code that is invalid, expired or already used.
//! 3. The page keeps the token in `sessionStorage` (key [`TOKEN_STORAGE_KEY`]): a reload of that
//!    tab keeps working, a new tab needs a new code. The token is the daemon secret, so it stays
//!    valid across daemon restarts until the secret is rotated.
//! 4. It opens `/ws` offering the subprotocols `[WS_PROTOCOL, WS_TOKEN_PROTOCOL_PREFIX + token]`.
//!    The daemon selects [`WS_PROTOCOL`] and never echoes the token.
//! 5. A browser cannot see the HTTP status of a refused upgrade: a 401 looks exactly like a daemon
//!    that is not running (close code 1006). So an upgrade that offers [`WS_PROTOCOL`] with a
//!    missing or wrong token is accepted and immediately closed with [`WS_CLOSE_UNAUTHORIZED`]
//!    before any request is read. The page then forgets its token and asks for `pulse open`
//!    instead of retrying. Any other close means "reconnect with backoff".
//!
//! `Host` and `Origin` are checked on every request first (see the daemon's transport module);
//! the login exchange needs no token but is subject to the same checks.

pub use crate::status::{UiLoginRequest, UiLoginResponse};

/// The URL fragment parameter that carries a login code: `#login=<code>`.
pub const LOGIN_FRAGMENT_PARAM: &str = "login";
/// `POST` a [`UiLoginRequest`] here for a [`UiLoginResponse`].
pub const LOGIN_PATH: &str = "/auth/login";
/// How long a login code stays valid, in seconds.
pub const LOGIN_CODE_TTL_SECS: u64 = 120;
/// The WebSocket path for JSON-RPC.
pub const WS_PATH: &str = "/ws";
/// The subprotocol the daemon selects. Offered by browsers alongside the token protocol.
pub const WS_PROTOCOL: &str = "pulse";
/// A browser offers the token as the subprotocol `pulse.token.<token>`.
pub const WS_TOKEN_PROTOCOL_PREFIX: &str = "pulse.token.";
/// The close code for a WebSocket whose token is missing or wrong. Retrying will not help.
pub const WS_CLOSE_UNAUTHORIZED: u16 = 4401;
/// Where the web UI keeps the token (`sessionStorage`, per tab).
pub const TOKEN_STORAGE_KEY: &str = "pulse.token";

/// The constants above as TypeScript, for the generated bindings.
pub(crate) fn typescript_constants() -> String {
    let s = |name: &str, v: &str| format!("export const {name} = {v:?};\n");
    let n = |name: &str, v: u64| format!("export const {name} = {v};\n");
    [
        "/** How the web UI authenticates; see `pulse_contracts::auth`. */\n".to_owned(),
        s("LOGIN_FRAGMENT_PARAM", LOGIN_FRAGMENT_PARAM),
        s("LOGIN_PATH", LOGIN_PATH),
        n("LOGIN_CODE_TTL_SECS", LOGIN_CODE_TTL_SECS),
        s("WS_PATH", WS_PATH),
        s("WS_PROTOCOL", WS_PROTOCOL),
        s("WS_TOKEN_PROTOCOL_PREFIX", WS_TOKEN_PROTOCOL_PREFIX),
        n("WS_CLOSE_UNAUTHORIZED", u64::from(WS_CLOSE_UNAUTHORIZED)),
        s("TOKEN_STORAGE_KEY", TOKEN_STORAGE_KEY),
    ]
    .concat()
}
