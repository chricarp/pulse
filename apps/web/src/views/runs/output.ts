// A run's output as it streams in: chunks in `seq` order, appended in place, with a version
// number for views. Updates are coalesced to one per animation frame, so a flood of chunks
// costs a bounded number of renders.

import { useEffect, useState, useSyncExternalStore } from "react";

import type { OutputKind, RunOutputChunk } from "../../contracts";
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
