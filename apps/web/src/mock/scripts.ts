// Scripted agent output, shaped like what the adapters produce: Claude Code and Codex output
// is structured (thinking, tool use, results), `command` output is plain stdout and stderr.

import type { AgentConfig, OutputKind } from "../contracts";

export interface ScriptStep {
  /** Milliseconds after the previous step. */
  delay: number;
  kind: OutputKind;
  text: string;
  data?: unknown;
}

export interface Script {
  steps: ScriptStep[];
  outcome: "succeeded" | "failed" | "timed_out";
  exitCode: number | null;
}

let toolSeq = 0;

function tool(name: string, input: Record<string, unknown>, result: string, isError = false): ScriptStep[] {
  const id = `toolu_${(++toolSeq).toString(36).padStart(6, "0")}`;
  const summary = Object.values(input)
    .filter((v) => typeof v === "string")
    .join(" ");
  return [
    { delay: 500, kind: "tool_use", text: `${name} ${summary}`.trim(), data: { id, name, input } },
    { delay: 700, kind: "tool_result", text: result, data: { tool_use_id: id, is_error: isError } },
  ];
}

export function scriptFor(agent: AgentConfig, subject: string, rule: string | null): Script {
  if (agent.adapter === "command") {
    const prog = agent.command[0] ?? "command";
    if (prog === "prettier") {
      return {
        steps: [{ delay: 300, kind: "stdout", text: `${subject.split("/").pop() ?? subject} 41ms` }],
        outcome: "succeeded",
        exitCode: 0,
      };
    }
    return {
      steps: [
        { delay: 200, kind: "stdout", text: "Latexmk: This is Latexmk, John Collins, 7 Apr. 2025. Version 4.86." },
        { delay: 400, kind: "stdout", text: "Rule 'pdflatex': File changes, etc:\n   Changed files, or newly in use since previous run(s):\n      chapters/03-method.tex" },
        { delay: 900, kind: "stderr", text: "LaTeX Warning: Citation `knuth1984' on page 12 undefined on input line 88." },
        { delay: 600, kind: "stdout", text: "Output written on main.pdf (48 pages, 1203381 bytes)." },
      ],
      outcome: "succeeded",
      exitCode: 0,
    };
  }
  if (agent.adapter === "codex") {
    return {
      steps: [
        { delay: 300, kind: "system", text: "codex session started · model gpt-5-codex · sandbox workspace-write", data: { type: "session.created" } },
        { delay: 800, kind: "thinking", text: "The test `watcher::coalesces_bursts` failed with a timeout. Look at the test and the coalescer's timer handling." },
        ...tool("shell", { command: "cargo test -p pulse-sources watcher::coalesces_bursts -- --nocapture" }, "test watcher::coalesces_bursts ... FAILED\n\nthread 'watcher::coalesces_bursts' panicked at crates/pulse-sources/src/fs/coalesce.rs:212:9:\nassertion `left == right` failed\n  left: 2\n right: 1", true),
        ...tool("read_file", { path: "crates/pulse-sources/src/fs/coalesce.rs" }, "(212 lines)"),
        { delay: 1200, kind: "assistant", text: "The quiet-period timer is re-armed from the first signal instead of the latest one, so two writes 1.9s apart become two events. The fix is to reset `deadline` on every signal for a pending path." },
        ...tool("apply_patch", { path: "crates/pulse-sources/src/fs/coalesce.rs" }, "Patch applied: 1 file changed, 3 insertions(+), 2 deletions(-)"),
        ...tool("shell", { command: "cargo test -p pulse-sources watcher::" }, "test result: ok. 14 passed; 0 failed; 0 ignored"),
        { delay: 600, kind: "result", text: "Fixed: the coalescer now restarts the quiet period on each signal. 14 watcher tests pass. Not committed.", data: { turns: 6 } },
      ],
      outcome: "succeeded",
      exitCode: 0,
    };
  }
  // claude-code
  const name = subject.split("/").pop() ?? subject;
  const failing = rule === "sync-notes";
  return {
    steps: [
      { delay: 300, kind: "system", text: "Claude Code session · model sonnet · permission mode acceptEdits · tools Read, Write, Edit", data: { subtype: "init", session_id: "f3c1e7a2" } },
      { delay: 900, kind: "thinking", text: `I need to read ${name} first, then produce a summary and flashcards. The PDF is a lecture, so the structure probably follows the slides.` },
      ...tool("Read", { file_path: subject }, `Read 38 pages (482 KB) from ${name}`),
      { delay: 1500, kind: "assistant", text: `## ${name}\n\n- Pipelining splits instruction execution into stages (IF, ID, EX, MEM, WB).\n- Hazards: structural, data (RAW, WAR, WAW) and control.\n- Forwarding resolves most RAW hazards without stalls; load-use still needs one bubble.\n- Branch prediction: static (not taken), 1-bit and 2-bit saturating counters.\n- CPI with stalls = 1 + stall cycles per instruction.` },
      { delay: 900, kind: "thinking", text: "Now the flashcards. Keep them atomic: one fact per card." },
      ...tool("Write", { file_path: `${subject}.cards.md` }, failing ? "Error: EACCES: permission denied" : "Wrote 24 cards (3.1 KB)", failing),
      { delay: 800, kind: failing ? "error" : "assistant", text: failing ? "Could not write the cards file: permission denied." : "Wrote 24 flashcards next to the lecture." },
      {
        delay: 400,
        kind: "result",
        text: failing ? "Stopped: the cards file could not be written." : `Summary and 24 flashcards for ${name}.`,
        data: { duration_ms: 9800, num_turns: 4, total_cost_usd: 0.0412 },
      },
    ],
    outcome: failing ? "failed" : "succeeded",
    exitCode: failing ? 1 : 0,
  };
}

/** A very long, verbose run: a test suite with every line logged. */
export function bigOutput(targetBytes: number): { kind: OutputKind; text: string }[] {
  const out: { kind: OutputKind; text: string }[] = [];
  let bytes = 0;
  let i = 0;
  const modules = ["engine", "router", "store::events", "store::runs", "sources::fs", "sources::git", "agents::supervisor", "mcp::tools"];
  while (bytes < targetBytes) {
    i += 1;
    const m = modules[i % modules.length] ?? "engine";
    let text: string;
    let kind: OutputKind = "stdout";
    if (i % 997 === 0) {
      kind = "stderr";
      text = `warning: test ${m}::case_${i} took 2.${i % 10}s (slow)`;
    } else if (i % 211 === 0) {
      text = `     Running tests/${m.replace("::", "_")}.rs (target/debug/deps/${m.replace("::", "_")}-${(i * 2654435761 % 0xffffffff).toString(16)})\n\nrunning 128 tests`;
    } else {
      text = `test ${m}::case_${String(i).padStart(6, "0")} ... ok   [${(i % 97) + 1}.${i % 10}ms] checked ${i % 13} invariants over ${(i * 7) % 1000} events`;
    }
    out.push({ kind, text });
    bytes += text.length + 1;
  }
  return out;
}
