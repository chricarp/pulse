//! `pulse mcp --agent <name>` (the stdio bridge) and the text of
//! `pulse agents mcp-credentials <agent>`.
//!
//! The bridge is `pulse_mcp::stdio::proxy` to the daemon's `/mcp` with the agent's token: one
//! MCP server (the daemon's), one place that decides who the caller is. Stdout carries MCP
//! messages only; anything else goes to stderr.
//!
//! - The token is `PULSE_MCP_TOKEN` if set (so the MCP client's config holds only an agent
//!   token), otherwise it is derived from `$PULSE_HOME/token`, which the bridge reads but never
//!   sends.
//! - Before each connection carries the token, the daemon must prove that it knows it
//!   (`pulse_contracts::auth`); a program that cannot is never sent the token.
//! - Before serving, it presents the token once: no daemon is exit 3, an agent the daemon does
//!   not know (or a token for another agent) is exit 1, each with a message on stderr. Failing
//!   at launch is what MCP clients show the user.
//! - Once serving, a daemon that stops is reported per request as a JSON-RPC error, and the
//!   bridge keeps running: when the daemon comes back on the same port, requests work again.
//!   It exits 0 when stdin closes.

use std::process::ExitCode;
use std::sync::Arc;

use pulse_contracts::rpc::McpCredentials;
use pulse_daemon::PulseHome;
use pulse_mcp::stdio::{CheckError, Target, check, proxy};

use super::Res;
use crate::cli::McpArgs;
use crate::{Ctx, Failure, eprint_line};

/// The environment variable the bridge takes an agent token from.
pub const TOKEN_ENV: &str = "PULSE_MCP_TOKEN";

pub async fn bridge(ctx: &Ctx, args: McpArgs) -> Res {
    let home = PulseHome::new(&ctx.home);
    let not_running = || {
        Failure::NotRunning(format!(
            "pulse mcp: the Pulse daemon for {} is not running; start it with `pulse start`",
            ctx.home.display()
        ))
    };
    let info = home.read_daemon_info().ok().flatten().ok_or_else(not_running)?;
    let token = match std::env::var(TOKEN_ENV) {
        Ok(token) => {
            if !token.starts_with(&format!("pa.{}.", args.agent)) {
                eprint_line(&format!(
                    "pulse mcp: {TOKEN_ENV} is not a token for agent {:?}; get one with `pulse agents mcp-credentials {}`",
                    args.agent, args.agent
                ));
                return Err(Failure::Exit(2));
            }
            token
        }
        Err(_) => {
            let secret = home.read_token().map_err(|e| {
                Failure::Message(format!(
                    "pulse mcp: cannot read {}: {e}; or set {TOKEN_ENV}",
                    home.token_file().display()
                ))
            })?;
            pulse_daemon::auth::agent_token(&secret, &args.agent)
        }
    };
    let url = format!("http://127.0.0.1:{}/mcp", info.port);
    // Whatever listens on the port must prove it knows the agent token before it is sent.
    let expected = token.clone();
    let target = Target::new(&url, token).map_err(|e| Failure::Message(e.to_string()))?.with_proof(
        &args.agent,
        Arc::new(move |nonce, answer, port| {
            pulse_daemon::auth::proof_matches(&expected, port, nonce, answer)
        }),
    );
    match check(&target).await {
        Ok(()) => {}
        Err(CheckError::Unreachable { .. }) => return Err(not_running()),
        Err(e @ CheckError::NotPulse(_)) => {
            return Err(Failure::Message(format!(
                "pulse mcp: {e} If you set {TOKEN_ENV}, it may also be stale (the daemon's token was rotated): get a fresh one with `pulse agents mcp-credentials {}`.",
                args.agent
            )));
        }
        Err(CheckError::Rejected) => {
            return Err(Failure::Message(format!(
                "pulse mcp: the daemon does not accept this token for agent {:?}: the agent is not configured (see `pulse agents`), or the token is stale",
                args.agent
            )));
        }
    }
    proxy(target, tokio::io::stdin(), tokio::io::stdout())
        .await
        .map_err(|e| Failure::Message(format!("pulse mcp: {e}")))?;
    Ok(ExitCode::SUCCESS)
}

/// `pulse agents mcp-credentials`: the endpoint, the token, and configuration for the MCP
/// clients Pulse users run (commands as in `docs/internals/0007-mcp-and-sep-3415.md`).
pub fn credentials_text(ctx: &Ctx, c: &McpCredentials) -> String {
    let default_home = ctx.user_home.join(".pulse");
    let pulse = if ctx.home == default_home {
        "pulse".to_owned()
    } else {
        format!("pulse --home {}", shell_word(&ctx.home.display().to_string()))
    };
    let (agent, url, token) = (shell_word(&c.agent), &c.url, &c.token);
    format!(
        "\
url    {url}
token  {token}

The token works only on /mcp and only as {agent}: it cannot approve runs or edit rules. It is
not a defence against programs running as you, which can read $PULSE_HOME/token.

Claude Code, stdio (nothing secret in its config; the bridge derives the token):
  claude mcp add pulse -- {pulse} mcp --agent {agent}

Claude Code, HTTP (the header is stored in its config in plain text):
  claude mcp add --transport http pulse {url} --header \"Authorization: Bearer {token}\"

Codex CLI, HTTP (token from the environment):
  export {TOKEN_ENV}={token}
  codex mcp add pulse --url {url} --bearer-token-env-var {TOKEN_ENV}

Codex CLI, stdio:
  codex mcp add pulse -- {pulse} mcp --agent {agent}

A checked-in .mcp.json must not contain the token:
  {{\"mcpServers\": {{\"pulse\": {{\"type\": \"http\", \"url\": \"{url}\", \"headers\": {{\"Authorization\": \"Bearer ${{{TOKEN_ENV}}}\"}}}}}}}}

The stdio bridge also takes the token from {TOKEN_ENV} instead of reading $PULSE_HOME/token."
    )
}

/// Quotes a word for a POSIX shell when it needs it.
fn shell_word(s: &str) -> String {
    let plain =
        !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-+:=@%,".contains(&b));
    if plain { s.to_owned() } else { format!("'{}'", s.replace('\'', r"'\''")) }
}
