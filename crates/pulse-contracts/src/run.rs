//! Runs, run output, routing decisions and deliveries.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::config::{AgentConfig, RuleConfig};
use crate::ids::{DeliveryId, EventId, RunId};
use crate::time::Timestamp;

/// ```text
/// pending_approval ─approve─▶ queued ─▶ running ─▶ succeeded | failed | timed_out | cancelled
///        ├─reject─▶ rejected     │                └─ daemon died ─▶ interrupted
///        └─cancel─▶ cancelled    ├─cancel─▶ cancelled
///                                └─spawn failed─▶ failed
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    PendingApproval,
    Queued,
    Running,
    Succeeded,
    Failed,
    TimedOut,
    Cancelled,
    Rejected,
    Interrupted,
}

impl RunStatus {
    pub const ALL: [RunStatus; 9] = [
        RunStatus::PendingApproval,
        RunStatus::Queued,
        RunStatus::Running,
        RunStatus::Succeeded,
        RunStatus::Failed,
        RunStatus::TimedOut,
        RunStatus::Cancelled,
        RunStatus::Rejected,
        RunStatus::Interrupted,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            RunStatus::PendingApproval => "pending_approval",
            RunStatus::Queued => "queued",
            RunStatus::Running => "running",
            RunStatus::Succeeded => "succeeded",
            RunStatus::Failed => "failed",
            RunStatus::TimedOut => "timed_out",
            RunStatus::Cancelled => "cancelled",
            RunStatus::Rejected => "rejected",
            RunStatus::Interrupted => "interrupted",
        }
    }

    pub fn parse(s: &str) -> Option<RunStatus> {
        RunStatus::ALL.into_iter().find(|st| st.as_str() == s)
    }

    pub const fn is_terminal(self) -> bool {
        !matches!(self, RunStatus::PendingApproval | RunStatus::Queued | RunStatus::Running)
    }

    /// The legal state machine. Terminal states have no way out; `runs.rerun` makes a new run.
    pub const fn can_transition_to(self, next: RunStatus) -> bool {
        use RunStatus::*;
        matches!(
            (self, next),
            (PendingApproval, Queued | Rejected | Cancelled)
                | (Queued, Running | Cancelled | Failed)
                | (Running, Succeeded | Failed | TimedOut | Cancelled | Interrupted)
        )
    }
}

/// A run in full, as `runs.get` returns it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(optional_fields = nullable)]
pub struct Run {
    pub id: RunId,
    /// `None` only for runs whose rule was deleted before a rerun.
    pub rule: Option<String>,
    pub agent: String,
    pub event: EventId,
    pub status: RunStatus,
    pub created_at: Timestamp,
    pub started_at: Option<Timestamp>,
    pub ended_at: Option<Timestamp>,
    /// The rendered prompt, exactly as sent.
    pub prompt: Option<String>,
    /// What the run executes, rendered when the run is created (so an approval shows it) from
    /// the snapshot; the run starts only if rendering it again gives the same result and the
    /// agent is still defined as in the snapshot. `None` if it could not be rendered (the run
    /// then fails to start).
    pub invocation: Option<InvocationSummary>,
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    /// Why the run failed to start, was rejected, or was interrupted.
    pub error: Option<String>,
    /// 1 for a run triggered by an uncaused event; one more than the causing run's otherwise.
    pub chain_depth: u32,
    pub rerun_of: Option<RunId>,
    pub output: OutputStats,
    /// The rule and agent as configured when it was created. A run never starts with an agent
    /// definition that differs from the current one: see `docs/internals/0002-architecture.md`.
    pub snapshot: RunSnapshot,
}

impl Run {
    pub fn summary(&self) -> RunSummary {
        RunSummary {
            id: self.id,
            rule: self.rule.clone(),
            agent: self.agent.clone(),
            event: self.event,
            status: self.status,
            created_at: self.created_at,
            started_at: self.started_at,
            ended_at: self.ended_at,
            exit_code: self.exit_code,
            chain_depth: self.chain_depth,
        }
    }
}

/// The list form of a run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(optional_fields = nullable)]
pub struct RunSummary {
    pub id: RunId,
    pub rule: Option<String>,
    pub agent: String,
    pub event: EventId,
    pub status: RunStatus,
    pub created_at: Timestamp,
    pub started_at: Option<Timestamp>,
    pub ended_at: Option<Timestamp>,
    pub exit_code: Option<i32>,
    pub chain_depth: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(optional_fields = nullable)]
pub struct RunSnapshot {
    pub rule: Option<RuleConfig>,
    pub agent: AgentConfig,
}

/// What was executed: argv and working directory. The environment is not recorded (it may hold
/// secrets from the agent's config).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct InvocationSummary {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct OutputStats {
    /// Bytes of output produced, including dropped bytes.
    #[ts(type = "number")]
    pub bytes: u64,
    /// Bytes dropped from the middle to respect the per-run cap.
    #[ts(type = "number")]
    pub dropped_bytes: u64,
}

/// One normalized piece of agent output. Provider detail is preserved in `data`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(optional_fields = nullable)]
pub struct RunOutput {
    pub kind: OutputKind,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "unknown")]
    pub data: Option<Value>,
}

impl RunOutput {
    pub fn new(kind: OutputKind, text: impl Into<String>) -> Self {
        RunOutput { kind, text: text.into(), data: None }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum OutputKind {
    /// Raw stdout line (agents without a structured format, or unparseable lines).
    Stdout,
    Stderr,
    /// Text the agent addressed to the user.
    Assistant,
    Thinking,
    ToolUse,
    ToolResult,
    /// Session and status information from the agent runtime.
    System,
    /// The final result or summary.
    Result,
    Error,
    /// Inserted by Pulse, for example where output was dropped.
    Marker,
}

impl OutputKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            OutputKind::Stdout => "stdout",
            OutputKind::Stderr => "stderr",
            OutputKind::Assistant => "assistant",
            OutputKind::Thinking => "thinking",
            OutputKind::ToolUse => "tool_use",
            OutputKind::ToolResult => "tool_result",
            OutputKind::System => "system",
            OutputKind::Result => "result",
            OutputKind::Error => "error",
            OutputKind::Marker => "marker",
        }
    }

    pub fn parse(s: &str) -> Option<OutputKind> {
        [
            OutputKind::Stdout,
            OutputKind::Stderr,
            OutputKind::Assistant,
            OutputKind::Thinking,
            OutputKind::ToolUse,
            OutputKind::ToolResult,
            OutputKind::System,
            OutputKind::Result,
            OutputKind::Error,
            OutputKind::Marker,
        ]
        .into_iter()
        .find(|k| k.as_str() == s)
    }
}

/// A stored output chunk. `seq` is per run, increasing, with gaps only where output was dropped.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct RunOutputChunk {
    #[ts(type = "number")]
    pub seq: u64,
    pub at: Timestamp,
    #[serde(flatten)]
    pub output: RunOutput,
}

/// A prompt ready to send: the trusted instruction followed by the fenced, untrusted event data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct RenderedPrompt {
    pub text: String,
    /// The per-run fence line around the data block.
    pub delimiter: String,
    /// Something was cut to respect a size limit.
    pub truncated: bool,
    /// Placeholders whose value was absent (rendered as empty).
    pub missing: Vec<String>,
}

/// What routing decided for one matching rule.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(optional_fields = nullable)]
pub enum Outcome {
    /// Create a queued run. `prompt` is `None` for a `command` agent whose rule has none.
    Fire {
        prompt: Option<RenderedPrompt>,
    },
    /// Create a run that waits for approval.
    AwaitApproval {
        prompt: Option<RenderedPrompt>,
    },
    /// Held by the rule's debounce until `until`; the latest event then fires.
    Debounced {
        until: Timestamp,
    },
    Blocked {
        reason: BlockReason,
    },
}

impl Outcome {
    pub fn delivery(&self) -> DeliveryOutcome {
        match self {
            Outcome::Fire { .. } => DeliveryOutcome::Fired,
            Outcome::AwaitApproval { .. } => DeliveryOutcome::AwaitingApproval,
            Outcome::Debounced { until } => DeliveryOutcome::Debounced { until: *until },
            Outcome::Blocked { reason } => DeliveryOutcome::Blocked { reason: reason.clone() },
        }
    }

    pub fn creates_run(&self) -> bool {
        matches!(self, Outcome::Fire { .. } | Outcome::AwaitApproval { .. })
    }
}

/// Why a matching rule did not fire.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum BlockReason {
    RuleDisabled,
    RuleSnoozed {
        until: Timestamp,
    },
    GlobalPause,
    /// The agent's scopes do not cover the event. `needed` is a scope that would.
    PermissionDenied {
        needed: String,
    },
    /// The event was caused by a run of this same rule.
    LoopSelfTrigger {
        run: RunId,
    },
    /// Firing would make a chain of more than `max` runs.
    LoopChainDepth {
        depth: u32,
        max: u32,
    },
    /// The circuit breaker tripped (now or earlier) and holds the rule until `until`.
    CircuitOpen {
        until: Timestamp,
    },
    Cooldown {
        until: Timestamp,
    },
    RateLimited {
        until: Timestamp,
    },
    /// The agent already has `backlog` runs queued or running, its limit.
    AgentBusy {
        backlog: u32,
        max: u32,
    },
    /// The prompt could not be rendered.
    RenderFailed {
        message: String,
    },
}

/// One decision from `route()`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Decision {
    pub rule: String,
    pub agent: String,
    pub outcome: Outcome,
    /// The chain depth a run created by this decision gets.
    pub chain_depth: u32,
}

/// The stored form of a decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(optional_fields = nullable)]
pub struct Delivery {
    pub id: DeliveryId,
    pub event: EventId,
    pub rule: String,
    pub agent: String,
    pub at: Timestamp,
    pub outcome: DeliveryOutcome,
    pub run: Option<RunId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeliveryOutcome {
    Fired,
    AwaitingApproval,
    Debounced { until: Timestamp },
    Blocked { reason: BlockReason },
}

impl DeliveryOutcome {
    pub const fn kind_str(&self) -> &'static str {
        match self {
            DeliveryOutcome::Fired => "fired",
            DeliveryOutcome::AwaitingApproval => "awaiting_approval",
            DeliveryOutcome::Debounced { .. } => "debounced",
            DeliveryOutcome::Blocked { .. } => "blocked",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_state_machine() {
        use RunStatus::*;
        let legal = [
            (PendingApproval, Queued),
            (PendingApproval, Rejected),
            (PendingApproval, Cancelled),
            (Queued, Running),
            (Queued, Cancelled),
            (Queued, Failed),
            (Running, Succeeded),
            (Running, Failed),
            (Running, TimedOut),
            (Running, Cancelled),
            (Running, Interrupted),
        ];
        for from in RunStatus::ALL {
            for to in RunStatus::ALL {
                assert_eq!(
                    from.can_transition_to(to),
                    legal.contains(&(from, to)),
                    "{from:?} -> {to:?}"
                );
            }
            assert_eq!(RunStatus::parse(from.as_str()), Some(from));
            if from.is_terminal() {
                assert!(RunStatus::ALL.iter().all(|to| !from.can_transition_to(*to)));
            }
        }
    }

    #[test]
    fn outcomes_serialize_with_kind_and_reason_tags() {
        let o = Outcome::Blocked { reason: BlockReason::LoopChainDepth { depth: 4, max: 3 } };
        let json = serde_json::to_value(&o).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"kind": "blocked", "reason": {"reason": "loop_chain_depth", "depth": 4, "max": 3}})
        );
        assert_eq!(serde_json::from_value::<Outcome>(json).unwrap(), o);
        assert_eq!(o.delivery().kind_str(), "blocked");
    }
}
