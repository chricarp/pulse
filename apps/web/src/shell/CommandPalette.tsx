import { useEffect, useRef, useState } from "react";

import { useApp, useDaemon } from "../app/context";
import { pendingApprovals } from "../app/daemon";
import { navigate } from "../app/router";
import { cycleTheme } from "../app/theme";
import { attempt, toast } from "../app/toasts";
import { RUN_STATUS_LABEL } from "../lib/explain";
import { relative, shortId, visible } from "../lib/format";
import { Icon, type IconName } from "../ui/icons";
import { Kbd, cx } from "../ui/primitives";
import { routeFor, useTogglePause } from "./Shell";

interface Command {
  id: string;
  group: string;
  label: string;
  detail?: string;
  icon: IconName;
  keys?: string;
  run: () => void;
}

/** Subsequence match with a bonus for word starts and contiguous runs; `null` if no match. */
export function score(query: string, text: string): number | null {
  if (!query) return 0;
  const q = query.toLowerCase();
  const t = text.toLowerCase();
  let qi = 0;
  let s = 0;
  let run = 0;
  for (let i = 0; i < t.length && qi < q.length; i++) {
    if (t[i] === q[qi]) {
      const start = i === 0 || /[\s/._:-]/.test(t[i - 1] ?? "");
      run += 1;
      s += 1 + run + (start ? 4 : 0);
      qi++;
    } else run = 0;
  }
  return qi === q.length ? s - t.length * 0.01 : null;
}

export function CommandPalette({ open, onClose, onHelp }: { open: boolean; onClose: () => void; onHelp: () => void }) {
  const app = useApp();
  const { client, mock } = app;
  const runs = useDaemon((s) => s.runs);
  const rules = useDaemon((s) => s.rules);
  const agents = useDaemon((s) => s.agents);
  const sources = useDaemon((s) => s.sources);
  const paused = useDaemon((s) => s.status?.paused ?? false);
  const togglePause = useTogglePause();
  const [query, setQuery] = useState("");
  const [index, setIndex] = useState(0);
  const dialog = useRef<HTMLDialogElement>(null);
  const input = useRef<HTMLInputElement>(null);
  const list = useRef<HTMLUListElement>(null);

  useEffect(() => {
    const d = dialog.current;
    if (!d) return;
    if (open && !d.open) {
      setQuery("");
      setIndex(0);
      d.showModal();
      input.current?.focus();
    }
    if (!open && d.open) d.close();
  }, [open]);

  if (!open) {
    return <dialog ref={dialog} className="palette" onClose={onClose} />;
  }

  const commands: Command[] = [];
  const nav = (label: string, view: Parameters<typeof routeFor>[0], keys: string, icon: IconName) =>
    commands.push({ id: `nav-${view}`, group: "Go to", label, icon, keys, run: () => navigate(routeFor(view)) });
  nav("Feed", "feed", "g f", "feed");
  nav("Runs", "runs", "g u", "runs");
  nav("Rules", "rules", "g r", "rules");
  nav("Agents", "agents", "g a", "agents");
  nav("Sources", "sources", "g s", "sources");
  nav("Projects", "projects", "g p", "projects");
  nav("System", "system", "g y", "system");

  for (const r of pendingApprovals(runs)) {
    commands.push({
      id: `approve-${r.id}`,
      group: "Awaiting approval",
      label: `Approve ${r.rule ?? "run"} → ${r.agent}`,
      detail: `created ${relative(r.created_at, Date.now())}`,
      icon: "check",
      run: () => void attempt("Approve", () => client.call("runs.approve", { id: r.id }), "Approved"),
    });
    commands.push({ id: `review-${r.id}`, group: "Awaiting approval", label: `Review ${r.rule ?? "run"} → ${r.agent}`, icon: "shield", run: () => navigate({ view: "runs", runId: r.id }) });
  }

  commands.push(
    { id: "pause", group: "Actions", label: paused ? "Resume everything" : "Pause everything", icon: paused ? "play" : "pause", keys: "shift+p", run: togglePause },
    { id: "new-rule", group: "Actions", label: "New rule", icon: "plus", keys: "n", run: () => navigate({ view: "rules", name: null, mode: "new" }) },
    { id: "new-agent", group: "Actions", label: "New agent", icon: "plus", run: () => navigate({ view: "agents", name: null, mode: "new" }) },
    { id: "new-source", group: "Actions", label: "New source", icon: "plus", run: () => navigate({ view: "sources", name: null, mode: "new" }) },
    {
      id: "filter",
      group: "Actions",
      label: "Filter the feed",
      icon: "filter",
      keys: "/",
      run: () => {
        navigate(routeFor("feed"), { keepSearch: true });
        requestAnimationFrame(() => document.getElementById("feed-query")?.focus());
      },
    },
    { id: "theme", group: "Actions", label: "Switch theme", detail: "system, light, dark", icon: "sun", keys: "shift+t", run: () => toast(`Theme: ${cycleTheme()}`, "info") },
    { id: "help", group: "Actions", label: "Keyboard shortcuts", icon: "key", keys: "?", run: onHelp },
  );

  const sorted = [...runs.values()].sort((a, b) => (a.id < b.id ? 1 : -1)).slice(0, 40);
  for (const r of sorted) {
    commands.push({
      id: `run-${r.id}`,
      group: "Runs",
      label: `${r.rule ?? "(deleted rule)"} → ${r.agent}`,
      detail: `${RUN_STATUS_LABEL[r.status]} · ${relative(r.created_at, Date.now())} · ${shortId(r.id)}`,
      icon: "runs",
      run: () => navigate({ view: "runs", runId: r.id }),
    });
  }
  for (const r of rules ?? []) {
    commands.push({ id: `rule-${r.config.name}`, group: "Rules", label: r.config.name, detail: r.config.archived ? "archived" : r.config.on.join(", "), icon: "rules", run: () => navigate({ view: "rules", name: r.config.name, mode: "show" }) });
  }
  for (const a of agents ?? []) {
    commands.push({ id: `agent-${a.config.name}`, group: "Agents", label: a.config.name, detail: a.config.adapter, icon: "agents", run: () => navigate({ view: "agents", name: a.config.name, mode: "show" }) });
  }
  for (const s of sources ?? []) {
    commands.push({ id: `source-${s.config.name}`, group: "Sources", label: s.config.name, detail: `${s.config.kind} · ${s.health.state}`, icon: "sources", run: () => navigate({ view: "sources", name: s.config.name, mode: "show" }) });
  }
  if (mock) {
    commands.push(
      { id: "mock-storm", group: "Mock daemon", label: "Event storm: 10,000 events in 3 seconds", icon: "alert", run: () => mock.storm(10_000, 3) },
      { id: "mock-big", group: "Mock daemon", label: "Run with 6 MB of output", icon: "runs", run: () => navigate({ view: "runs", runId: mock.bigRun() }) },
      { id: "mock-approval", group: "Mock daemon", label: "Trigger a supervised rule", icon: "shield", run: () => mock.requestApproval() },
      { id: "mock-drop", group: "Mock daemon", label: "Drop the connection", icon: "x", run: () => mock.dropConnection() },
      { id: "mock-stop", group: "Mock daemon", label: "Stop the daemon", icon: "stop", run: () => mock.stopDaemon() },
      { id: "mock-start", group: "Mock daemon", label: "Start the daemon", icon: "play", run: () => mock.startDaemon() },
    );
  }

  const q = query.trim();
  const shown = q
    ? commands
        .map((c) => ({ c, s: score(q, `${c.label} ${c.group}`) }))
        .filter((x): x is { c: Command; s: number } => x.s !== null)
        .sort((a, b) => b.s - a.s)
        .slice(0, 50)
        .map((x) => x.c)
    : commands.filter((c) => c.group !== "Runs" || commands.indexOf(c) < commands.findIndex((x) => x.group === "Runs") + 8).slice(0, 60);
  const sel = Math.min(index, Math.max(0, shown.length - 1));

  const execute = (c: Command | undefined) => {
    if (!c) return;
    onClose();
    c.run();
  };

  let lastGroup = "";
  return (
    <dialog
      ref={dialog}
      className="palette"
      aria-label="Command palette"
      onClose={onClose}
      onCancel={(e) => {
        e.preventDefault();
        onClose();
      }}
      onMouseDown={(e) => {
        if (e.target === dialog.current) onClose();
      }}
    >
      <div className="palette-inner">
        <div className="palette-input">
          <Icon name="search" />
          <input
            ref={input}
            value={query}
            placeholder="Go to a view, run, rule, agent or source, or run a command"
            aria-label="Search commands"
            aria-controls="palette-list"
            aria-activedescendant={shown[sel] ? `cmd-${shown[sel].id}` : undefined}
            role="combobox"
            aria-expanded="true"
            spellCheck={false}
            onChange={(e) => {
              setQuery(e.target.value);
              setIndex(0);
            }}
            onKeyDown={(e) => {
              if (e.key === "ArrowDown" || (e.ctrlKey && e.key === "n")) {
                e.preventDefault();
                setIndex((i) => Math.min(i + 1, shown.length - 1));
                list.current?.children[Math.min(sel + 1, shown.length - 1)]?.scrollIntoView({ block: "nearest" });
              } else if (e.key === "ArrowUp" || (e.ctrlKey && e.key === "p")) {
                e.preventDefault();
                setIndex((i) => Math.max(i - 1, 0));
                list.current?.children[Math.max(sel - 1, 0)]?.scrollIntoView({ block: "nearest" });
              } else if (e.key === "Enter") {
                e.preventDefault();
                execute(shown[sel]);
              }
            }}
          />
          <Kbd keys="escape" subtle />
        </div>
        <ul className="palette-list" id="palette-list" role="listbox" ref={list}>
          {shown.length === 0 && <li className="palette-empty">Nothing matches “{visible(q)}”.</li>}
          {shown.map((c, i) => {
            const header = c.group !== lastGroup ? c.group : null;
            lastGroup = c.group;
            return (
              <li
                key={c.id}
                id={`cmd-${c.id}`}
                role="option"
                aria-selected={i === sel}
                className={cx("palette-item", i === sel && "is-selected")}
                data-group={header ?? undefined}
                onMouseMove={() => setIndex(i)}
                onClick={() => execute(c)}
              >
                {header && <span className="palette-group">{header}</span>}
                <Icon name={c.icon} size={14} />
                <span className="palette-label">{visible(c.label, { singleLine: true })}</span>
                {c.detail && <span className="palette-detail">{visible(c.detail, { singleLine: true })}</span>}
                {c.keys && <Kbd keys={c.keys} subtle />}
              </li>
            );
          })}
        </ul>
      </div>
    </dialog>
  );
}
