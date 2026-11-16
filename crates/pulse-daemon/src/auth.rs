//! Credentials: the daemon token, per-agent MCP tokens, and single-use UI login codes.
//!
//! - The **daemon token** is the secret in `$PULSE_HOME/token`. It grants full control and is
//!   accepted on `/ws` and `/ingest`.
//! - An **agent token** is `pa.<agent>.<hex HMAC-SHA256(secret, "pulse/agent-token/v1\0" +
//!   agent)>`. It is accepted only on `/mcp`, as that agent, so an agent's MCP configuration never
//!   holds a credential that can approve runs or edit rules. Rotating the secret revokes all.
//! - A **login code** is `<expiry ms>.<hex HMAC(secret, "pulse/ui-login/v1\0" + expiry)>`, valid
//!   until its expiry and only once. `pulse open` puts it in the URL fragment; the UI exchanges it
//!   at `POST /auth/login` for the token. The token itself never appears in process arguments
//!   or browser history.
//!
//! None of these is ever logged.

use std::collections::VecDeque;
use std::sync::Mutex;

use hmac::{Hmac, KeyInit, Mac};
use pulse_contracts::{Duration, Timestamp};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

const AGENT_TOKEN_PREFIX: &str = "pa.";
const AGENT_CONTEXT: &[u8] = b"pulse/agent-token/v1\0";
const LOGIN_CONTEXT: &[u8] = b"pulse/ui-login/v1\0";
/// How long a login code from `pulse open` stays valid.
pub const LOGIN_CODE_TTL: Duration = Duration::from_secs(120);
const MAX_USED_CODES: usize = 256;

pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[(b >> 4) as usize] as char);
        s.push(DIGITS[(b & 0xf) as usize] as char);
    }
    s
}

/// Compares secrets without an early exit on the first differing byte.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn mac(secret: &str, context: &[u8], message: &[u8]) -> String {
    // HMAC accepts keys of any length, so construction cannot fail.
    let mut m = match HmacSha256::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return String::new(),
    };
    m.update(context);
    m.update(message);
    hex(&m.finalize().into_bytes())
}

/// The MCP token for `agent`.
pub fn agent_token(secret: &str, agent: &str) -> String {
    format!("{AGENT_TOKEN_PREFIX}{agent}.{}", mac(secret, AGENT_CONTEXT, agent.as_bytes()))
}

/// The agent an agent token was issued for, if the token is genuine. Whether the agent still
/// exists is the caller's question.
pub fn verify_agent_token<'t>(secret: &str, token: &'t str) -> Option<&'t str> {
    let rest = token.strip_prefix(AGENT_TOKEN_PREFIX)?;
    let (agent, tag) = rest.rsplit_once('.')?;
    let want = mac(secret, AGENT_CONTEXT, agent.as_bytes());
    constant_time_eq(want.as_bytes(), tag.as_bytes()).then_some(agent)
}

/// A login code valid until `now + LOGIN_CODE_TTL`. Clients that can read the token file (the
/// CLI) mint these themselves.
pub fn login_code(secret: &str, now: Timestamp) -> String {
    let expiry = now.saturating_add(LOGIN_CODE_TTL).as_millis();
    format!("{expiry}.{}", mac(secret, LOGIN_CONTEXT, expiry.to_string().as_bytes()))
}

/// Remembers redeemed login codes until they expire, so each works once. Bounded: past the
/// bound, the oldest are forgotten (they expire within minutes anyway).
#[derive(Debug, Default)]
pub(crate) struct LoginCodes {
    used: Mutex<VecDeque<(i64, String)>>,
}

impl LoginCodes {
    pub(crate) fn redeem(&self, secret: &str, code: &str, now: Timestamp) -> bool {
        let Some((expiry, tag)) = code.split_once('.') else { return false };
        let Ok(expiry_ms) = expiry.parse::<i64>() else { return false };
        let want = mac(secret, LOGIN_CONTEXT, expiry.as_bytes());
        if !constant_time_eq(want.as_bytes(), tag.as_bytes()) {
            return false;
        }
        let ttl = i64::try_from(LOGIN_CODE_TTL.as_millis()).unwrap_or(i64::MAX);
        if expiry_ms <= now.as_millis() || expiry_ms > now.as_millis().saturating_add(ttl) {
            return false;
        }
        let mut used = self.used.lock().unwrap_or_else(|p| p.into_inner());
        used.retain(|(exp, _)| *exp > now.as_millis());
        if used.iter().any(|(_, c)| c == code) {
            return false;
        }
        if used.len() >= MAX_USED_CODES {
            used.pop_front();
        }
        used.push_back((expiry_ms, code.to_owned()));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_tokens_are_bound_to_secret_and_agent() {
        let t = agent_token("s1", "proj/study");
        assert_eq!(verify_agent_token("s1", &t), Some("proj/study"));
        assert_eq!(verify_agent_token("s2", &t), None);
        let forged = t.replace("proj/study", "admin");
        assert_eq!(verify_agent_token("s1", &forged), None);
        assert_eq!(verify_agent_token("s1", "s1"), None, "the daemon token is not an agent token");
    }

    #[test]
    fn login_codes_expire_and_work_once() {
        let codes = LoginCodes::default();
        let t0 = Timestamp::from_millis(1_000_000);
        let code = login_code("s", t0);
        assert!(!codes.redeem("other", &code, t0));
        assert!(codes.redeem("s", &code, t0));
        assert!(!codes.redeem("s", &code, t0), "single use");
        let late = login_code("s", t0);
        assert!(!codes.redeem("s", &late, t0.saturating_add(LOGIN_CODE_TTL)));
        assert!(!codes.redeem("s", "garbage", t0));
    }
}
