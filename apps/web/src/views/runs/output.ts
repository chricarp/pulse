// A run's output as it streams in: chunks in `seq` order, appended in place, with a version
// number for views. Updates are coalesced to one per animation frame, so a flood of chunks
// costs a bounded number of renders.

import { useEffect, useState, useSyncExternalStore } from "react";

import type { OutputKind, RunOutputChunk } from "../../contracts";
import { visible } from "../../lib/format";
import type { RpcClient } from "../../rpc/client";

export interface OutputSnapshot {
  chunks: readonly RunOutputChunk[];
  done: boolean;
  version: number;
  error: string | null;
  connected: boolean;
}

export class OutputStream {
  private chunks: RunOutputChunk[] = [];
  private lastSeq = 0;
  private done = false;
  private error: string | null = null;
  private connected = false;
  private version = 0;
  private snapshot: OutputSnapshot;
  private readonly listeners = new Set<() => void>();
  private frame: number | null = null;
  private close: (() => void) | null = null;

  constructor(client: RpcClient, runId: string) {
    this.snapshot = this.make();
    const sub = client.subscribe(
      "runs.subscribe_output",
      // After a reconnect, resume after the last chunk received.
      () => ({ id: runId, after: this.lastSeq }),
      (n) => {
        this.append(n.chunks);
        if (n.done) {
          this.done = true;
          this.schedule();
          // Nothing more will come.
          sub.close();
        }
      },
      {
        onSubscribed: () => {
          this.connected = true;
          this.schedule();
        },
        onLost: () => {
          this.connected = false;
          this.schedule();
        },
        onError: (e) => {
          this.error = e.message;
          this.schedule();
        },
      },
    );
    this.close = () => sub.close();
  }

  dispose(): void {
    this.close?.();
    if (this.frame !== null) cancelAnimationFrame(this.frame);
  }

  private append(chunks: readonly RunOutputChunk[]): void {
    for (const c of chunks) {
      // Ordered by seq; a resubscription may resend the edge.
      if (c.seq <= this.lastSeq) continue;
      this.chunks.push(c);
      this.lastSeq = c.seq;
    }
    this.schedule();
  }

  private make(): OutputSnapshot {
    return { chunks: this.chunks, done: this.done, version: this.version, error: this.error, connected: this.connected };
  }

  private schedule(): void {
    if (this.frame !== null) return;
    this.frame = requestAnimationFrame(() => {
      this.frame = null;
      this.version += 1;
      this.snapshot = this.make();
      for (const l of this.listeners) l();
    });
  }

  subscribe = (l: () => void): (() => void) => {
    this.listeners.add(l);
    return () => this.listeners.delete(l);
  };

  get = (): OutputSnapshot => this.snapshot;
}

export function useRunOutput(client: RpcClient, runId: string): OutputSnapshot {
  const [stream, setStream] = useState<OutputStream | null>(null);
  useEffect(() => {
    const s = new OutputStream(client, runId);
    setStream(s);
    return () => s.dispose();
  }, [client, runId]);
  const empty: OutputSnapshot = { chunks: [], done: false, version: 0, error: null, connected: false };
  return useSyncExternalStore(stream?.subscribe ?? noopSubscribe, stream?.get ?? (() => empty));
}

const noopSubscribe = () => () => {};

// ---- layout: must match the `.ch` rules in styles.css ----

/** Line height of output text, px. */
export const LINE = 18;
const COLLAPSE_LINES = 8;
const MAX_INLINE_CHARS = 20_000;

function expandTabs(text: string): string {
  if (!text.includes("\t")) return text;
  return text
    .split("\n")
    .map((line) => {
      let out = "";
      for (const ch of line) out += ch === "\t" ? " ".repeat(8 - (out.length % 8)) : ch;
      return out;
    })
    .join("\n");
}

/** The text a chunk shows (hidden characters made visible, tabs expanded, long ones clipped). */
export function displayText(chunk: RunOutputChunk, expanded: boolean): { text: string; clipped: boolean; lines: number } {
  let text = expandTabs(visible(chunk.text));
  const all = text.split("\n");
  let clipped = false;
  if (!expanded && chunk.kind === "tool_result" && all.length > COLLAPSE_LINES) {
    text = all.slice(0, COLLAPSE_LINES).join("\n");
    clipped = true;
  }
  if (!expanded && text.length > MAX_INLINE_CHARS) {
    text = text.slice(0, MAX_INLINE_CHARS);
    clipped = true;
  }
  return { text, clipped, lines: all.length };
}

function wrappedLines(text: string, width: number, charWidth: number): number {
  const per = width > 0 ? Math.max(1, Math.floor(width / charWidth)) : 120;
  let n = 0;
  let start = 0;
  for (;;) {
    const end = text.indexOf("\n", start);
    const len = (end < 0 ? text.length : end) - start;
    n += Math.max(1, Math.ceil(len / per));
    if (end < 0) return n;
    start = end + 1;
  }
}

// The virtualizer asks for every row's height whenever the row count changes; cache them.
const layoutCache = new WeakMap<RunOutputChunk, { key: string; height: number; exact: boolean }>();

// Characters whose width in a monospace font is one cell. Anything else (CJK, emoji, symbols
// from a fallback font) makes the computed height a guess, so that row is measured instead.
const PROSE: ReadonlySet<OutputKind> = new Set(["assistant", "thinking", "result", "error", "system"]);
const ONE_CELL = /^[\n\u0020-\u007E\u00A0-\u024F]*$/;

/** The pixel height of a chunk's row for a container `width` and a monospace `charWidth`. */
export function chunkLayout(chunk: RunOutputChunk, expanded: boolean, width: number, charWidth: number): { height: number; exact: boolean } {
  const key = `${width}|${charWidth}|${expanded ? 1 : 0}`;
  const hit = layoutCache.get(chunk);
  if (hit?.key === key) return hit;
  const computed = computeLayout(chunk, expanded, width, charWidth);
  // Prose (what the agent says, thinks, concludes) wraps at words in the text font, so its
  // height is a guess and the row is measured. Those rows are few next to plain output.
  const prose = PROSE.has(chunk.kind);
  const exact = !prose && (chunk.kind === "marker" || chunk.kind === "tool_use" || ONE_CELL.test(displayText(chunk, expanded).text));
  const entry = { key, height: computed.height, exact };
  layoutCache.set(chunk, entry);
  return entry;
}

function computeLayout(chunk: RunOutputChunk, expanded: boolean, width: number, charWidth: number): { height: number } {
  switch (chunk.kind) {
    case "marker":
      return { height: 36 };
    case "tool_use":
      return { height: 32 };
    default:
      break;
  }
  const { text, clipped } = displayText(chunk, expanded);
  const more = clipped ? LINE : 0;
  const pad = 32; // 16px each side
  switch (chunk.kind) {
    case "tool_result":
      return { height: 6 + 1 + 6 + more + LINE * wrappedLines(text, width - pad - 2 - 16, charWidth) };
    case "result":
    case "error":
      return { height: 8 + 10 + LINE + more + LINE * wrappedLines(text, width - pad - 3 - 20, charWidth) };
    case "assistant":
      return { height: 10 + more + LINE * wrappedLines(text, width - pad, charWidth) };
    case "thinking":
      return { height: 6 + more + LINE * wrappedLines(text, width - pad, charWidth) };
    default:
      return { height: 2 + more + LINE * wrappedLines(text, width - pad, charWidth) };
  }
}

export const KIND_GROUPS: { key: string; label: string; kinds: OutputKind[] }[] = [
  { key: "thinking", label: "Thinking", kinds: ["thinking"] },
  { key: "tools", label: "Tool calls", kinds: ["tool_use", "tool_result"] },
  { key: "system", label: "System", kinds: ["system"] },
];

/** Tool name and a short argument summary from a `tool_use` chunk's provider data. */
export function toolSummary(chunk: RunOutputChunk): { name: string; args: string } {
  const d = chunk.data as { name?: unknown; input?: unknown } | null | undefined;
  const name = typeof d?.name === "string" ? d.name : chunk.text.split(" ")[0] ?? "tool";
  let args = "";
  if (d && typeof d.input === "object" && d.input !== null) {
    args = Object.entries(d.input as Record<string, unknown>)
      .map(([k, v]) => `${k}: ${typeof v === "string" ? v : JSON.stringify(v)}`)
      .join("  ");
  } else {
    args = chunk.text.slice(name.length).trim();
  }
  return { name, args };
}

export function isToolError(chunk: RunOutputChunk): boolean {
  const d = chunk.data as { is_error?: unknown } | null | undefined;
  return d?.is_error === true;
}
