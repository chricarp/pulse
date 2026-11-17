// Words for routing outcomes and run states: what happened, and why, in one line.

import type { BlockReason, Causation, DeliveryOutcome, EventOutcome, Outcome, RunStatus, SourceState, TrustState } from "../contracts";

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
    case "waiting":
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
    case "waiting":
      return `waiting: ${blockLabel(o.reason)}`;
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

/**
 * One sentence about a decision, true whenever it is read: what became of a run it created is
 * the run's state, shown next to it. `superseded`: a later delivery of the same rule for the
 * same event says what happened next (a debounce that elapsed, a waiting delivery routed again).
 */
export function outcomeSentence(o: DeliveryOutcome | Outcome, agent: string, when: (iso: string) => string, superseded = false): string {
  switch (o.kind) {
    case "fired":
    case "fire":
      return `Woke ${agent}.`;
    case "awaiting_approval":
    case "await_approval":
      return `The rule is supervised: a run of ${agent} starts only once you approve it.`;
    case "debounced":
      return superseded
        ? `Held by the debounce until ${when(o.until)}, then routed again (below).`
        : `Held by the debounce until ${when(o.until)}; if no newer matching event comes, it fires then.`;
    case "waiting":
      return superseded
        ? `${blockSentence(o.reason, agent, when)} The debounce held the event until one of ${agent}'s runs ended, then routed it again (below).`
        : `${blockSentence(o.reason, agent, when)} The debounce holds the event and fires it when one of ${agent}'s runs ends, unless a newer matching event replaces it.`;
    case "blocked":
      return blockSentence(o.reason, agent, when);
  }
}

/** What one rule's delivery for an event amounts to now, for a marker. */
export interface CurrentOutcome {
  rule: string;
  label: string;
  tone: Tone;
  /** The latest delivery of that rule for the event. */
  outcome: EventOutcome;
}

/**
 * Per rule, the latest delivery for the event (a debounce that elapsed or a waiting delivery
 * routed again is followed by the delivery that says what then happened), shown as the state
 * of the run it created when it created one: an approved run no longer reads "awaiting
 * approval". In the order the rules first decided.
 */
export function currentOutcomes(list: readonly EventOutcome[]): CurrentOutcome[] {
  const latest = new Map<string, EventOutcome>();
  for (const o of [...list].sort((a, b) => (a.delivery < b.delivery ? -1 : a.delivery > b.delivery ? 1 : 0))) latest.set(o.rule, o);
  return [...latest.values()].map((o) =>
    o.run && o.run_status
      ? { rule: o.rule, label: RUN_STATUS_LABEL[o.run_status], tone: runTone(o.run_status), outcome: o }
      : { rule: o.rule, label: deliveryLabel(o.outcome), tone: deliveryTone(o.outcome), outcome: o },
  );
}

/** How an event's causing run is known: a pill, a feed flag, and the explanation. */
export const CAUSATION: Record<Causation, { label: string; flag: string; tone: Tone; why: string }> = {
  exact: {
    label: "exact",
    flag: "by agent",
    tone: "ok",
    why: "The run reported this event itself (it carried the run's PULSE_RUN_ID).",
  },
  reported: {
    label: "reported",
    flag: "by agent",
    tone: "ok",
    why: "The agent's own tool call named this file (a Write or Edit in its output), so Pulse attributes the change to the run. It is as reliable as the agent's output.",
  },
  inferred: {
    label: "inferred",
    flag: "by agent?",
    tone: "waiting",
    why: "The change happened inside that run's working directory while it ran (or just after), and the run had done something whose writes Pulse cannot see (a shell command, say), so Pulse attributes the change to the run. Filesystems do not say which process wrote a file, so this is a guess.",
  },
};

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
