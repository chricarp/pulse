import { useVirtualizer } from "@tanstack/react-virtual";
import { memo, useLayoutEffect, useRef, useState } from "react";

import { useApp } from "../../app/context";
import { useBindings } from "../../app/keys";
import type { RunOutputChunk } from "../../contracts";
import { bytes, clockTime, count, visible } from "../../lib/format";
import { Icon } from "../../ui/icons";
import { Button, Empty, cx } from "../../ui/primitives";
import { KIND_GROUPS, isToolError, toolSummary, useRunOutput } from "./output";

const COLLAPSE_LINES = 8;
const MAX_INLINE_CHARS = 20_000;

export function RunOutput({ runId, live, droppedBytes, totalBytes }: { runId: string; live: boolean; droppedBytes: number; totalBytes: number }) {
  const { client } = useApp();
  const out = useRunOutput(client, runId);
  const [hidden, setHidden] = useState<Set<string>>(() => new Set());
  const [expanded, setExpanded] = useState<Set<number>>(() => new Set());
  const [following, setFollowing] = useState(true);
  const parent = useRef<HTMLDivElement>(null);

  const hiddenKinds = new Set(KIND_GROUPS.filter((g) => hidden.has(g.key)).flatMap((g) => g.kinds));
  // Indices into `out.chunks` that are shown. Recomputed when output or filters change.
  const shown: number[] = [];
  for (let i = 0; i < out.chunks.length; i++) {
    const c = out.chunks[i];
    if (c && !hiddenKinds.has(c.kind)) shown.push(i);
  }

  const virtualizer = useVirtualizer({
    count: shown.length,
    getScrollElement: () => parent.current,
    estimateSize: () => 20,
    overscan: 20,
    getItemKey: (i) => out.chunks[shown[i] ?? 0]?.seq ?? i,
  });

  // Follow the tail: after new output renders, stay pinned to the bottom.
  useLayoutEffect(() => {
    if (!following || shown.length === 0) return;
    virtualizer.scrollToIndex(shown.length - 1, { align: "end" });
  }, [out.version, following, shown.length, virtualizer]);

  const jumpToEnd = () => {
    setFollowing(true);
    virtualizer.scrollToIndex(shown.length - 1, { align: "end" });
  };
  useBindings([
    { keys: "end", label: "Follow the output", group: "Runs", run: jumpToEnd },
    { keys: "shift+g", label: "Follow the output", group: "Runs", run: jumpToEnd, hidden: true },
  ]);

  const all = () => out.chunks.map((c) => c.text).join("\n");
  const toggleKind = (key: string) =>
    setHidden((h) => {
      const n = new Set(h);
      if (n.has(key)) n.delete(key);
      else n.add(key);
      return n;
    });

  return (
    <div className="output">
      <div className="output-bar">
        <span className="faint num">
          {count(out.chunks.length)} chunks · {bytes(totalBytes)}
          {droppedBytes > 0 && <span className="tone-text-attention"> · {bytes(droppedBytes)} dropped from the middle (output cap)</span>}
        </span>
        <span className="spacer" />
        {KIND_GROUPS.map((g) => (
          <label key={g.key} className="output-kind">
            <input type="checkbox" checked={!hidden.has(g.key)} onChange={() => toggleKind(g.key)} />
            {g.label}
          </label>
        ))}
        <Button size="sm" variant="ghost" onClick={() => void navigator.clipboard?.writeText(all())} title="Copy the output loaded so far">
          Copy
        </Button>
      </div>
      <div
        ref={parent}
        className="output-scroll"
        tabIndex={0}
        role="log"
        aria-label="Run output"
        aria-live="off"
        onScroll={(e) => {
          const el = e.currentTarget;
          const atEnd = el.scrollHeight - el.scrollTop - el.clientHeight < 32;
          if (atEnd !== following) setFollowing(atEnd);
        }}
      >
        {shown.length === 0 ? (
          <Empty title={out.done ? "No output" : live ? "Waiting for output…" : out.connected ? "No output yet" : "Loading output…"}>
            {out.error && <p className="tone-text-attention">{out.error}</p>}
          </Empty>
        ) : (
          <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
            {virtualizer.getVirtualItems().map((v) => {
              const chunk = out.chunks[shown[v.index] ?? 0];
              if (!chunk) return null;
              return (
                <div key={v.key} data-index={v.index} ref={virtualizer.measureElement} className="vout" style={{ transform: `translateY(${v.start}px)` }}>
                  <Chunk
                    chunk={chunk}
                    expanded={expanded.has(chunk.seq)}
                    onExpand={() => setExpanded((s) => new Set(s).add(chunk.seq))}
                  />
                </div>
              );
            })}
          </div>
        )}
      </div>
      {!following && shown.length > 0 && (
        <button type="button" className="follow-btn" onClick={jumpToEnd}>
          <Icon name="arrowDown" size={12} />
          {live && !out.done ? "Follow live output" : "Jump to the end"}
          <span className="kbd-hint">End</span>
        </button>
      )}
      {live && !out.done && following && <div className="output-live" aria-hidden="true">live</div>}
    </div>
  );
}

const Chunk = memo(function Chunk({ chunk, expanded, onExpand }: { chunk: RunOutputChunk; expanded: boolean; onExpand: () => void }) {
  const time = clockTime(chunk.at, true);
  if (chunk.kind === "marker") {
    return (
      <div className="ch ch-marker" title={time}>
        <span>{visible(chunk.text, { singleLine: true })}</span>
      </div>
    );
  }
  if (chunk.kind === "tool_use") {
    const { name, args } = toolSummary(chunk);
    return (
      <div className="ch ch-tool" title={time}>
        <Icon name="tool" size={12} />
        <span className="ch-tool-name">{visible(name, { singleLine: true })}</span>
        <span className="ch-tool-args">{visible(args, { singleLine: true })}</span>
      </div>
    );
  }
  let text = chunk.text;
  let clipped = false;
  const lines = text.split("\n");
  const collapsible = chunk.kind === "tool_result" && lines.length > COLLAPSE_LINES;
  if (!expanded && collapsible) {
    text = lines.slice(0, COLLAPSE_LINES).join("\n");
    clipped = true;
  }
  if (!expanded && text.length > MAX_INLINE_CHARS) {
    text = text.slice(0, MAX_INLINE_CHARS);
    clipped = true;
  }
  return (
    <div className={cx("ch", `ch-${chunk.kind}`, chunk.kind === "tool_result" && isToolError(chunk) && "is-error")} title={time}>
      {chunk.kind === "result" && <span className="ch-label">Result</span>}
      {chunk.kind === "error" && <span className="ch-label">Error</span>}
      <span className="ch-text">{visible(text)}</span>
      {clipped && (
        <button type="button" className="link-btn ch-more" onClick={onExpand}>
          Show all ({collapsible ? `${count(lines.length)} lines` : bytes(chunk.text.length)})
        </button>
      )}
    </div>
  );
});
