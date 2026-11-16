// Words for routing outcomes and run states: what happened, and why, in one line.

import type { BlockReason, DeliveryOutcome, Outcome, RunStatus, SourceState, TrustState } from "../contracts";

export type Tone = "ok" | "attention" | "waiting" | "loop" | "blocked" | "error" | "neutral" | "running";

export function isLoopProtection(r: BlockReason): boolean {
  return r.reason === "loop_self_trigger" || r.reason === "loop_chain_depth" || r.reason === "circuit_open";
}

export function deliveryTone(o: DeliveryOutcome | Outcome): Tone {
  switch (o.kind) {
    case "fired":
    case "fire":
      return "ok";
    case "awaiting_approval":
    case "await_approval":
      return "attention";
    case "debounced":
      return "waiting";
    case "blocked":
      return isLoopProtection(o.reason) ? "loop" : "blocked";
  }
}

export function deliveryLabel(o: DeliveryOutcome | Outcome): string {
  switch (o.kind) {
    case "fired":
    case "fire":
      return "fired";
    case "awaiting_approval":
    case "await_approval":
      return "awaiting approval";
    case "debounced":
      return "debounced";
    case "blocked":
      return blockLabel(o.reason);
  }
}

export function blockLabel(r: BlockReason): string {
  switch (r.reason) {
    case "rule_disabled":
      return "rule disabled";
    case "rule_snoozed":
      return "snoozed";
    case "global_pause":
      return "paused";
    case "permission_denied":
      return "out of scope";
    case "loop_self_trigger":
      return "loop: own run";
    case "loop_chain_depth":
      return "loop: chain too deep";
    case "circuit_open":
      return "circuit breaker";
    case "cooldown":
      return "cooldown";
    case "rate_limited":
      return "rate limited";
    case "agent_busy":
      return "agent busy";
    case "render_failed":
      return "prompt failed";
  }
}

/** One sentence; `when` formats a timestamp. */
export function blockSentence(r: BlockReason, agent: string, when: (iso: string) => string): string {
  switch (r.reason) {
    case "rule_disabled":
      return "The rule is disabled.";
    case "rule_snoozed":
      return `The rule is snoozed until ${when(r.until)}.`;
    case "global_pause":
      return "Pulse was paused. Rules are evaluated and recorded while paused, but nothing fires.";
    case "permission_denied":
      return `Agent ${agent} is not allowed to see this event. A scope like ${r.needed} would cover it.`;
    case "loop_self_trigger":
      return `Loop protection: this event was caused by a run of this same rule, so the rule did not fire on it again.`;
    case "loop_chain_depth":
      return `Loop protection: firing would make a chain of ${r.depth} runs, more than the limit of ${r.max}.`;
    case "circuit_open":
      return `Circuit breaker: the rule fired too often in a short time and is held until ${when(r.until)}. Unsnooze the rule to close it.`;
    case "cooldown":
      return `In cooldown after its last fire, until ${when(r.until)}.`;
    case "rate_limited":
      return `Rate limit reached; the next fire is allowed at ${when(r.until)}.`;
    case "agent_busy":
      return `Agent ${agent} is full: ${r.backlog} runs running or queued, its limit is ${r.max}.`;
    case "render_failed":
      return `The prompt could not be rendered: ${r.message}`;
  }
}

export function outcomeSentence(o: DeliveryOutcome | Outcome, agent: string, when: (iso: string) => string): string {
  switch (o.kind) {
    case "fired":
    case "fire":
      return `Woke ${agent}.`;
    case "awaiting_approval":
    case "await_approval":
      return `A run of ${agent} is waiting for approval (the rule is supervised).`;
    case "debounced":
      return `Held by the debounce until ${when(o.until)}; if no newer matching event comes, it fires then.`;
    case "blocked":
      return blockSentence(o.reason, agent, when);
  }
}

export const RUN_STATUS_LABEL: Record<RunStatus, string> = {
  pending_approval: "awaiting approval",
  queued: "queued",
  running: "running",
  succeeded: "succeeded",
  failed: "failed",
  timed_out: "timed out",
  cancelled: "cancelled",
  rejected: "rejected",
  interrupted: "interrupted",
};

export function runTone(s: RunStatus): Tone {
  switch (s) {
    case "pending_approval":
      return "attention";
    case "queued":
      return "waiting";
    case "running":
      return "running";
    case "succeeded":
      return "ok";
    case "failed":
    case "timed_out":
    case "interrupted":
      return "error";
    case "cancelled":
    case "rejected":
      return "neutral";
  }
}

export function sourceTone(s: SourceState): Tone {
  switch (s) {
    case "healthy":
      return "ok";
    case "degraded":
      return "attention";
    case "failed":
      return "error";
    case "starting":
      return "waiting";
    case "stopped":
    case "disabled":
      return "neutral";
  }
}

export function trustTone(s: TrustState): Tone {
  switch (s) {
    case "trusted":
      return "ok";
    case "changed":
      return "attention";
    case "missing":
      return "error";
    case "untrusted":
      return "neutral";
  }
}

export const TRUST_LABEL: Record<TrustState, string> = {
  trusted: "trusted",
  changed: "changed since trusted",
  missing: "file missing",
  untrusted: "not trusted",
};
