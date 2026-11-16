import { useVirtualizer } from "@tanstack/react-virtual";
import { memo, useCallback, useLayoutEffect, useMemo, useRef, useState } from "react";

import { useApp } from "../../app/context";
import { useBindings } from "../../app/keys";
import type { RunOutputChunk } from "../../contracts";
import { bytes, clockTime, count, visible } from "../../lib/format";
import { Icon } from "../../ui/icons";
import { Button, Empty, cx } from "../../ui/primitives";
import { KIND_GROUPS, chunkLayout, displayText, isToolError, toolSummary, useRunOutput, LINE } from "./output";

export function RunOutput({ runId, live, droppedBytes, totalBytes }: { runId: string; live: boolean; droppedBytes: number; totalBytes: number }) {
  const { client } = useApp();
  const out = useRunOutput(client, runId);
  const [hidden, setHidden] = useState<Set<string>>(() => new Set());
  const [expanded, setExpanded] = useState<Set<number>>(() => new Set());
  const [following, setFollowing] = useState(true);
  const parent = useRef<HTMLDivElement>(null);
  const probe = useRef<HTMLSpanElement>(null);
  const [metrics, setMetrics] = useState({ width: 0, charWidth: 7.2 });

  // Row heights are computed from the text (fixed line height, monospace, wrapping at any
  // character), not measured: measuring rows as they scroll by costs a synchronous re-render
  // per row, which made scrolling through 40k rows stutter. Rows the arithmetic cannot predict
  // (wide characters) are still measured.
  useLayoutEffect(() => {
    const el = parent.current;
    if (!el) return;
    const read = () => {
      const cw = probe.current ? probe.current.getBoundingClientRect().width / 100 : 7.2;
      setMetrics((m) => (m.width === el.clientWidth && m.charWidth === cw ? m : { width: el.clientWidth, charWidth: cw || 7.2 }));
    };
    read();
    const ro = new ResizeObserver(read);
    ro.observe(el);
    void document.fonts?.ready.then(read);
    return () => ro.disconnect();
  }, []);

  const shown = useMemo(() => {
    const hiddenKinds = new Set(KIND_GROUPS.filter((g) => hidden.has(g.key)).flatMap((g) => g.kinds));
    const idx: number[] = [];
    for (let i = 0; i < out.chunks.length; i++) {
      const c = out.chunks[i];
      if (c && !hiddenKinds.has(c.kind)) idx.push(i);
    }
    return idx;
    // `out.version` changes whenever chunks are appended (the array is appended in place).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [out.version, out.chunks, hidden]);

  // Stable callbacks: the virtualizer recomputes every row's position when they change, which
  // must happen when the rows change, not on every scroll-driven render.
  const chunks = out.chunks;
  const estimateSize = useCallback(
    (i: number) => {
      const c = chunks[shown[i] ?? 0];
      return c ? chunkLayout(c, expanded.has(c.seq), metrics.width, metrics.charWidth).height : LINE;
    },
    [chunks, shown, expanded, metrics],
  );
  const getItemKey = useCallback((i: number) => chunks[shown[i] ?? 0]?.seq ?? i, [chunks, shown]);
  const getScrollElement = useCallback(() => parent.current, []);
  const virtualizer = useVirtualizer({ count: shown.length, getScrollElement, estimateSize, overscan: 12, getItemKey });

  // New widths or expanded chunks change heights: recompute.
  useLayoutEffect(() => {
    virtualizer.measure();
  }, [metrics, expanded, hidden, virtualizer]);

  // Follow the tail: after new output renders, stay pinned to the bottom.
  useLayoutEffect(() => {
    if (!following || shown.length === 0) return;
    virtualizer.scrollToIndex(shown.length - 1, { align: "end" });
  }, [out.version, following, shown.length, virtualizer, metrics]);

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
        <Button size="sm" variant="ghost" icon="copy" onClick={() => void navigator.clipboard?.writeText(all())} title="Copy the output loaded so far">
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
        <span ref={probe} className="ch-probe" aria-hidden="true">
          {"0".repeat(100)}
        </span>
        {shown.length === 0 ? (
          <Empty title={out.done ? "No output" : live ? "Waiting for output…" : out.connected ? "No output yet" : "Loading output…"}>
            {out.error && <p className="tone-text-attention">{out.error}</p>}
          </Empty>
        ) : (
          <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
            {virtualizer.getVirtualItems().map((v) => {
              const chunk = out.chunks[shown[v.index] ?? 0];
              if (!chunk) return null;
              // Only rows whose height cannot be computed exactly are measured.
              const exact = chunkLayout(chunk, expanded.has(chunk.seq), metrics.width, metrics.charWidth).exact;
              return (
                <div key={v.key} data-index={v.index} ref={exact ? undefined : virtualizer.measureElement} className="vout" style={{ transform: `translateY(${v.start}px)` }}>
                  <Chunk
                    chunk={chunk}
                    expanded={expanded.has(chunk.seq)}
                    width={metrics.width}
                    charWidth={metrics.charWidth}
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

const Chunk = memo(function Chunk({ chunk, expanded, width, charWidth, onExpand }: { chunk: RunOutputChunk; expanded: boolean; width: number; charWidth: number; onExpand: () => void }) {
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
        <div className="ch-box">
          <Icon name="tool" size={12} />
          <span className="ch-tool-name">{visible(name, { singleLine: true })}</span>
          <span className="ch-tool-args">{visible(args, { singleLine: true })}</span>
        </div>
      </div>
    );
  }
  const layout = chunkLayout(chunk, expanded, width, charWidth);
  const text = displayText(chunk, expanded);
  const body = (
    <>
      {chunk.kind === "result" && <span className="ch-label">Result</span>}
      {chunk.kind === "error" && <span className="ch-label">Error</span>}
      <span className="ch-text">{text.text}</span>
      {text.clipped && (
        <button type="button" className="link-btn ch-more" onClick={onExpand}>
          Show all ({text.lines > 1 ? `${count(text.lines)} lines` : bytes(chunk.text.length)})
        </button>
      )}
    </>
  );
  const boxed = chunk.kind === "tool_result" || chunk.kind === "result" || chunk.kind === "error";
  return (
    <div className={cx("ch", `ch-${chunk.kind}`, chunk.kind === "tool_result" && isToolError(chunk) && "is-error")} title={time} data-height={layout.height}>
      {boxed ? <div className="ch-box">{body}</div> : body}
    </div>
  );
});
