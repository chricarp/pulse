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
//! - A **resume code** is `r.<hex HMAC(HMAC(secret, RESUME_KEY_LABEL), instance)>`: what a
//!   browser tab keeps across a reload instead of the token. It works only on the daemon
//!   instance (one start of the daemon) it was made for.
//! - A **proof** shows a client, before it sends one of these, that whatever answers on the port
//!   knows it: `hex(HMAC(HMAC(credential, PROOF_KEY_LABEL), port \0 instance \0 nonce))`. The
//!   computation is specified in [`pulse_contracts::auth`]; [`proof`] is the one Rust
//!   implementation, used by the daemon to answer and by Rust clients to check.
//!
//! None of these is ever logged.

use std::collections::VecDeque;
use std::sync::Mutex;

use hmac::{Hmac, KeyInit, Mac};
use pulse_contracts::auth::{DaemonProof, PROOF_KEY_LABEL, RESUME_CODE_PREFIX, RESUME_KEY_LABEL};
use pulse_contracts::{Duration, Timestamp};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

const AGENT_TOKEN_PREFIX: &str = "pa.";
const AGENT_CONTEXT: &[u8] = b"pulse/agent-token/v1\0";
const LOGIN_CONTEXT: &[u8] = b"pulse/ui-login/v1\0";
/// How long a login code from `pulse open` stays valid.
pub const LOGIN_CODE_TTL: Duration =
    Duration::from_secs(pulse_contracts::auth::LOGIN_CODE_TTL_SECS);
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

fn hmac_parts(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    // HMAC accepts keys of any length, so construction cannot fail.
    let Ok(mut m) = HmacSha256::new_from_slice(key) else { return [0; 32] };
    for p in parts {
        m.update(p);
    }
    m.finalize().into_bytes().into()
}

/// What a daemon on `port`, started as `instance`, answers to a proof request with `nonce` for
/// `credential` (the daemon token, an agent token or a resume code). Clients compute the same
/// and compare with [`proof_matches`].
pub fn proof(credential: &str, port: u16, instance: &str, nonce: &str) -> String {
    let key = hmac_parts(credential.as_bytes(), &[PROOF_KEY_LABEL.as_bytes()]);
    let port = port.to_string();
    hex(&hmac_parts(&key, &[port.as_bytes(), b"\0", instance.as_bytes(), b"\0", nonce.as_bytes()]))
}

/// Whether `answer` proves knowledge of `credential` for the daemon on `port`. Constant time.
pub fn proof_matches(credential: &str, port: u16, nonce: &str, answer: &DaemonProof) -> bool {
    let want = proof(credential, port, &answer.instance, nonce);
    constant_time_eq(want.as_bytes(), answer.proof.as_bytes())
}

/// `N` random bytes from the OS, as hex: a proof nonce (32), a daemon instance id (16).
pub fn random_hex<const N: usize>() -> std::io::Result<String> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(hex(&bytes))
}

/// The resume code of daemon instance `instance`. The web UI computes the same in the browser.
pub fn resume_code(secret: &str, instance: &str) -> String {
    let key = hmac_parts(secret.as_bytes(), &[RESUME_KEY_LABEL.as_bytes()]);
    format!("{RESUME_CODE_PREFIX}{}", hex(&hmac_parts(&key, &[instance.as_bytes()])))
}

/// Whether `code` is the resume code of this daemon instance. Constant time.
pub fn verify_resume_code(secret: &str, instance: &str, code: &str) -> bool {
    constant_time_eq(resume_code(secret, instance).as_bytes(), code.as_bytes())
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
///
/// The memory does not survive a restart, so codes minted before this daemon started are
/// refused: otherwise a code that was already spent would work again for the rest of its two
/// minutes after a restart.
#[derive(Debug)]
pub(crate) struct LoginCodes {
    used: Mutex<VecDeque<(i64, String)>>,
    started_at: Timestamp,
}

impl LoginCodes {
    pub(crate) fn new(started_at: Timestamp) -> Self {
        LoginCodes { used: Mutex::new(VecDeque::new()), started_at }
    }

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
        if expiry_ms.saturating_sub(ttl) < self.started_at.as_millis() {
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

    /// The same vector is checked by the web UI (`apps/web/src/rpc/proof.test.ts`) and the
    /// desktop shell (`apps/desktop/src/proof.test.ts`); all three must agree.
    #[test]
    fn proofs_and_resume_codes_match_the_shared_vector() {
        let (token, instance) =
            ("0123456789abcdef0123456789abcdef", "00112233445566778899aabbccddeeff");
        let nonce = "ab".repeat(32);
        let p = proof(token, 7477, instance, &nonce);
        assert_eq!(p, "53a4504b2c9efa0f0b23a3209f384d908c85147e725d1244d827c5ec32fb4a4a");
        assert_eq!(
            resume_code(token, instance),
            "r.f07e6833dc77755c78d65044a0a50a984a8623d97c7d3c813796a0c5d1816bdf"
        );
        assert_eq!(
            proof(&agent_token(token, "study"), 7477, instance, &nonce),
            "19df5cf6bd407668c997937f4be5056ba59dd80856f5c993709f45366b906a8f"
        );

        let answer = DaemonProof { instance: instance.to_owned(), proof: p.clone() };
        assert!(proof_matches(token, 7477, &nonce, &answer));
        // Another port (a relay from a program on another port), another nonce, another
        // credential: each fails.
        assert!(!proof_matches(token, 7478, &nonce, &answer));
        assert!(!proof_matches(token, 7477, &"cd".repeat(32), &answer));
        assert!(!proof_matches("another token, also 32 bytes long", 7477, &nonce, &answer));
        let other = DaemonProof { instance: "ff".repeat(16), proof: p };
        assert!(!proof_matches(token, 7477, &nonce, &other), "the instance is covered");

        assert!(verify_resume_code(token, instance, &resume_code(token, instance)));
        assert!(!verify_resume_code(token, &"ff".repeat(16), &resume_code(token, instance)));
        // A proof is never a credential: it does not verify as an agent token, a login code
        // or a resume code, whatever nonce it was asked for.
        let as_resume = format!("r.{}", proof(token, 7477, instance, &nonce));
        assert!(!verify_resume_code(token, instance, &as_resume));
    }

    #[test]
    fn login_codes_expire_and_work_once() {
        let t0 = Timestamp::from_millis(1_000_000);
        let codes = LoginCodes::new(t0);
        let code = login_code("s", t0);
        assert!(!codes.redeem("other", &code, t0));
        assert!(codes.redeem("s", &code, t0));
        assert!(!codes.redeem("s", &code, t0), "single use");
        let late = login_code("s", t0);
        assert!(!codes.redeem("s", &late, t0.saturating_add(LOGIN_CODE_TTL)));
        assert!(!codes.redeem("s", "garbage", t0));

        // Which codes were spent is not kept across a restart, so a restarted daemon refuses
        // every code minted before it started (`pulse open` again gives a new one).
        let t1 = t0.saturating_add(Duration::from_secs(1));
        let spent = login_code("s", t1);
        assert!(codes.redeem("s", &spent, t1));
        let restarted = LoginCodes::new(t0.saturating_add(Duration::from_secs(5)));
        assert!(!restarted.redeem("s", &spent, t0.saturating_add(Duration::from_secs(10))));
        let fresh = login_code("s", t0.saturating_add(Duration::from_secs(10)));
        assert!(restarted.redeem("s", &fresh, t0.saturating_add(Duration::from_secs(10))));
    }
}
