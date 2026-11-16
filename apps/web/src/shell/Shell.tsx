import { useEffect, useState, type ReactNode } from "react";

import iconUrl from "../../../../assets/icon.svg?url";

import { useApp, useDaemon } from "../app/context";
import { pendingApprovals } from "../app/daemon";
import { useBindings } from "../app/keys";
import { href, navigate, useRoute, type Route, type ViewName } from "../app/router";
import { cycleTheme, useTheme } from "../app/theme";
import { attempt, dismiss, toasts } from "../app/toasts";
import { span } from "../lib/format";
import { RUN_STATUS_LABEL } from "../lib/explain";
import { CLOSE_UNPROVEN } from "../rpc/client";
import { useStore } from "../store/store";
import { Icon, type IconName } from "../ui/icons";
import { Button, Kbd, cx, useTicker } from "../ui/primitives";
import { CommandPalette } from "./CommandPalette";
import { HelpOverlay } from "./HelpOverlay";

const NAV: { view: ViewName; label: string; icon: IconName; keys: string }[] = [
  { view: "feed", label: "Feed", icon: "feed", keys: "g f" },
  { view: "runs", label: "Runs", icon: "runs", keys: "g u" },
  { view: "rules", label: "Rules", icon: "rules", keys: "g r" },
  { view: "agents", label: "Agents", icon: "agents", keys: "g a" },
  { view: "sources", label: "Sources", icon: "sources", keys: "g s" },
  { view: "projects", label: "Projects", icon: "projects", keys: "g p" },
  { view: "system", label: "System", icon: "system", keys: "g y" },
];

export function routeFor(view: ViewName): Route {
  switch (view) {
    case "feed":
      return { view, eventId: null };
    case "runs":
      return { view, runId: null };
    case "rules":
    case "agents":
    case "sources":
      return { view, name: null, mode: "show" };
    default:
      return { view };
  }
}

export function useTogglePause(): () => void {
  const { client } = useApp();
  const paused = useDaemon((s) => s.status?.paused ?? false);
  return () => {
    void attempt(paused ? "Resume" : "Pause", () => client.call(paused ? "system.resume" : "system.pause", {}), paused ? "Resumed: rules fire again" : "Paused: nothing fires until you resume");
  };
}

export function Shell({ children }: { children: ReactNode }) {
  const route = useRoute();
  const [palette, setPalette] = useState(false);
  const [help, setHelp] = useState(false);
  const togglePause = useTogglePause();
  const runs = useDaemon((s) => s.runs);
  const pending = pendingApprovals(runs);
  const firstPending = pending[0];

  useBindings([
    { keys: "mod+k", label: "Command palette", group: "Navigation", run: () => setPalette((v) => !v) },
    { keys: "?", label: "Keyboard shortcuts", group: "Navigation", run: () => setHelp((v) => !v) },
    ...NAV.map((n) => ({ keys: n.keys, label: `Go to ${n.label}`, group: "Navigation" as const, run: () => navigate(routeFor(n.view)) })),
    {
      keys: "g i",
      label: "Next run awaiting approval",
      group: "Runs",
      run: () => (firstPending ? navigate({ view: "runs", runId: firstPending.id }) : navigate({ view: "runs", runId: null }, {})),
    },
    { keys: "shift+p", label: "Pause or resume everything", group: "Actions", run: togglePause },
    { keys: "n", label: "New rule", group: "Rules", run: () => navigate({ view: "rules", name: null, mode: "new" }) },
    { keys: "shift+t", label: "Switch theme (system, light, dark)", group: "Actions", run: () => cycleTheme() },
  ]);

  // The title carries what needs attention, for when the window is in the background.
  useEffect(() => {
    document.title = pending.length ? `(${pending.length}) Pulse · approval needed` : "Pulse";
  }, [pending.length]);

  return (
    <div className="app">
      <Sidebar active={route.view} onPalette={() => setPalette(true)} />
      <div className="main">
        <Banners />
        <main className="view" id="main">
          {children}
        </main>
      </div>
      <CommandPalette open={palette} onClose={() => setPalette(false)} onHelp={() => setHelp(true)} />
      <HelpOverlay open={help} onClose={() => setHelp(false)} />
      <Toasts />
    </div>
  );
}

function Sidebar({ active, onPalette }: { active: ViewName; onPalette: () => void }) {
  const status = useDaemon((s) => s.status);
  const runs = useDaemon((s) => s.runs);
  const sources = useDaemon((s) => s.sources);
  const projects = useDaemon((s) => s.projects);
  const configErrors = useDaemon((s) => s.configErrors?.length ?? 0);
  const configWarnings = useDaemon((s) => s.configWarnings?.length ?? 0);
  let running = 0;
  let pending = 0;
  for (const r of runs.values()) {
    if (r.status === "running" || r.status === "queued") running++;
    if (r.status === "pending_approval") pending++;
  }
  const badSources = sources?.filter((s) => s.health.state === "failed" || s.health.state === "degraded").length ?? 0;
  const changedProjects = projects?.filter((p) => p.state === "changed" || p.state === "missing").length ?? 0;
  const badge = (view: ViewName): ReactNode => {
    switch (view) {
      case "runs":
        return (
          <>
            {pending > 0 && <span className="badge tone-attention" title={`${pending} awaiting approval`}>{pending}</span>}
            {running > 0 && <span className="badge tone-running" title={`${running} running or queued`}>{running}</span>}
          </>
        );
      case "rules":
        return (
          <>
            {configErrors > 0 && <span className="badge tone-error" title={`${configErrors} config errors`}>{configErrors}</span>}
            {configWarnings > 0 && <span className="badge tone-attention" title={`${configWarnings} definitions that cannot work as written`}>{configWarnings}</span>}
          </>
        );
      case "sources":
        return badSources > 0 ? <span className="badge tone-error" title={`${badSources} sources failing or degraded`}>{badSources}</span> : null;
      case "projects":
        return changedProjects > 0 ? <span className="badge tone-attention" title={`${changedProjects} need review`}>{changedProjects}</span> : null;
      default:
        return null;
    }
  };
  return (
    <nav className="sidebar" aria-label="Views">
      <div className="brand">
        <img src={iconUrl} alt="" width={20} height={20} />
        <span className="brand-name">Pulse</span>
        {status && <span className="brand-version num">{status.version}</span>}
      </div>
      <button type="button" className="palette-trigger" onClick={onPalette}>
        <Icon name="search" size={14} />
        <span>Go to…</span>
        <Kbd keys="mod+k" subtle />
      </button>
      <ul className="nav">
        {NAV.map((n) => (
          <li key={n.view}>
            <a
              href={href(routeFor(n.view))}
              className={cx("nav-item", active === n.view && "nav-active")}
              aria-current={active === n.view ? "page" : undefined}
              title={`${n.label} (${n.keys})`}
              onClick={(e) => {
                if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
                e.preventDefault();
                navigate(routeFor(n.view));
              }}
            >
              <Icon name={n.icon} />
              <span className="nav-label">{n.label}</span>
              <span className="nav-badges">{badge(n.view)}</span>
            </a>
          </li>
        ))}
      </ul>
      <div className="sidebar-foot">
        <PauseControl />
        <ConnectionLine />
        <ThemeButton />
      </div>
    </nav>
  );
}

function PauseControl() {
  const paused = useDaemon((s) => s.status?.paused ?? null);
  const open = useDaemon((s) => s.connection.kind === "open");
  const toggle = useTogglePause();
  if (paused === null) return null;
  return (
    <button
      type="button"
      className={cx("pause-control", paused && "is-paused")}
      onClick={toggle}
      disabled={!open}
      aria-pressed={paused}
      title={`${paused ? "Resume" : "Pause"} everything (Shift P)`}
    >
      <Icon name={paused ? "play" : "pause"} size={14} />
      <span className="pause-label">{paused ? "Resume all" : "Pause all"}</span>
      <Kbd keys="shift+p" subtle />
    </button>
  );
}

function ConnectionLine() {
  const c = useDaemon((s) => s.connection);
  const port = useDaemon((s) => s.status?.port);
  const label =
    c.kind === "open" ? `Connected${port ? ` · :${port}` : ""}` : c.kind === "connecting" ? "Connecting…" : c.kind === "waiting" ? "Reconnecting…" : c.kind === "unauthorized" ? "Not authorized" : "Disconnected";
  const tone = c.kind === "open" ? "ok" : c.kind === "unauthorized" ? "error" : "attention";
  return (
    <div className="conn-line" role="status" aria-live="polite">
      <span className={cx("dot", `tone-${tone}`)} aria-hidden="true" />
      <span>{label}</span>
    </div>
  );
}

function ThemeButton() {
  const theme = useTheme();
  const icon: IconName = theme === "light" ? "sun" : theme === "dark" ? "moon" : "monitor";
  return (
    <button type="button" className="theme-btn" onClick={() => cycleTheme()} title="Theme: system, light or dark (Shift T)" aria-label={`Theme: ${theme}. Switch`}>
      <Icon name={icon} size={14} />
      <span>{theme === "system" ? "System theme" : theme === "light" ? "Light" : "Dark"}</span>
    </button>
  );
}

function Banners() {
  return (
    <div className="banners">
      <ConnectionBanner />
      <PausedBanner />
      <ApprovalBanner />
    </div>
  );
}

function ConnectionBanner() {
  const { client } = useApp();
  const c = useDaemon((s) => s.connection);
  const failures = useDaemon((s) => s.failures);
  const everConnected = useDaemon((s) => s.status !== null);
  const now = useTicker(c.kind === "waiting");
  useEffect(() => {
    if (c.kind !== "waiting") return;
    // Retry at once when the user comes back or the network does.
    const retry = () => client.retryNow();
    window.addEventListener("focus", retry);
    window.addEventListener("online", retry);
    return () => {
      window.removeEventListener("focus", retry);
      window.removeEventListener("online", retry);
    };
  }, [c.kind, client]);
  if (c.kind === "open" || c.kind === "closed" || c.kind === "unauthorized") return null;
  if (c.kind === "connecting" && failures === 0) return null;
  const unproven = c.kind === "waiting" && c.code === CLOSE_UNPROVEN;
  const down = failures >= 3 || unproven;
  const wait = c.kind === "waiting" ? Math.max(0, c.retryAt - now) : 0;
  return (
    <div className={cx("banner", down ? "tone-error" : "tone-attention")} role="status" data-unproven={unproven || undefined}>
      <Icon name="alert" size={14} />
      <span className="banner-text">
        {unproven ? (
          <>
            <strong>Something on Pulse's port could not prove it is your daemon</strong>, so this window did not send it its token. Either another program took the port while the daemon was stopped (<code>pulse status</code> says so), or the daemon's token was rotated (run <code>pulse open</code>).
          </>
        ) : down ? (
          <>
            <strong>The daemon is not answering.</strong> Is it running? Start it with <code>pulse start</code>.
          </>
        ) : (
          <strong>Connection lost.</strong>
        )}{" "}
        {c.kind === "waiting" ? `Retrying in ${span(wait)}.` : "Connecting…"}
        {everConnected && " What you see is from before the connection dropped."}
      </span>
      {c.kind === "waiting" && (
        <Button size="sm" onClick={() => client.retryNow()}>
          Retry now
        </Button>
      )}
    </div>
  );
}

function PausedBanner() {
  const paused = useDaemon((s) => s.status?.paused ?? false);
  const toggle = useTogglePause();
  if (!paused) return null;
  return (
    <div className="banner tone-attention" role="status">
      <Icon name="pause" size={14} />
      <span className="banner-text">
        <strong>Paused.</strong> Events are recorded and rules are evaluated, but nothing fires.
      </span>
      <Button size="sm" variant="attention" icon="play" onClick={toggle} shortcut="shift+p">
        Resume
      </Button>
    </div>
  );
}

function ApprovalBanner() {
  const { client } = useApp();
  const runs = useDaemon((s) => s.runs);
  const route = useRoute();
  const pending = pendingApprovals(runs);
  const first = pending[0];
  if (!first) return null;
  // Already looking at it: the run view has the full approval card.
  if (route.view === "runs" && route.runId === first.id) return null;
  return (
    <div className="banner banner-approval tone-attention" role="status" aria-live="polite">
      <Icon name="shield" size={14} />
      <span className="banner-text">
        <strong>{pending.length === 1 ? "A run is waiting for your approval:" : `${pending.length} runs are waiting for your approval. First:`}</strong>{" "}
        <span className="mono">{first.rule ?? "(deleted rule)"}</span> → <span className="mono">{first.agent}</span>
      </span>
      <Button size="sm" variant="attention" onClick={() => navigate({ view: "runs", runId: first.id })} shortcut="g i">
        Review
      </Button>
      <Button
        size="sm"
        icon="check"
        onClick={() => void attempt("Approve", () => client.call("runs.approve", { id: first.id }), `Approved: ${first.rule ?? first.id} is ${RUN_STATUS_LABEL.queued}`)}
      >
        Approve
      </Button>
    </div>
  );
}

function Toasts() {
  const list = useStore(toasts, (t) => t);
  if (list.length === 0) return null;
  return (
    <div className="toasts" role="status" aria-live="polite">
      {list.map((t) => (
        <div key={t.id} className={cx("toast", `tone-${t.tone === "info" ? "neutral" : t.tone}`)}>
          <div>
            <strong>{t.message}</strong>
            {t.detail && <p>{t.detail}</p>}
          </div>
          <button type="button" aria-label="Dismiss" onClick={() => dismiss(t.id)}>
            <Icon name="x" size={12} />
          </button>
        </div>
      ))}
    </div>
  );
}
