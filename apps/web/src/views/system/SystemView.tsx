import { useState } from "react";

import { useApp, useDaemon } from "../../app/context";
import { setTheme, useTheme, type ThemeChoice } from "../../app/theme";
import { attempt } from "../../app/toasts";
import { count, dayAndTime, span } from "../../lib/format";
import { Button, Confirm, Dl, Empty, Notice, Section, Trunc, useNow } from "../../ui/primitives";
import { ShortcutList } from "../../shell/HelpOverlay";
import { useTogglePause } from "../../shell/Shell";
import { ConfigErrors, ConfigWarnings } from "../rules/RulesView";

export function SystemView() {
  const { client } = useApp();
  const status = useDaemon((s) => s.status);
  const connection = useDaemon((s) => s.connection);
  const errors = useDaemon((s) => s.configErrors);
  const warnings = useDaemon((s) => s.configWarnings);
  const theme = useTheme();
  const now = useNow();
  const togglePause = useTogglePause();
  const [shutdown, setShutdown] = useState(false);
  return (
    <div className="split">
      <div className="split-main">
        <header className="view-head">
          <h1>System</h1>
        </header>
        <div className="table-scroll view-pad stack system">
          {!status ? (
            <Empty title={connection.kind === "open" ? "Loading…" : "Not connected to the daemon"} />
          ) : (
            <>
              <Section title="Lictory Pulse">
                <div className="pause-panel">
                  <div>
                    <strong>{status.paused ? "Paused" : "Running"}</strong>
                    <p className="muted">
                      {status.paused
                        ? "Events are recorded and rules are evaluated, but nothing fires. Deliveries say “paused”."
                        : "Rules fire as events arrive. Pause to stop every rule at once without changing any of them."}
                    </p>
                  </div>
                  <Button variant={status.paused ? "attention" : "default"} icon={status.paused ? "play" : "pause"} onClick={togglePause} shortcut="shift+p">
                    {status.paused ? "Resume everything" : "Pause everything"}
                  </Button>
                </div>
                <Dl
                  items={[
                    ["Daemon", <span key="v" className="num">version {status.version} · up {span(now - Date.parse(status.started_at))} (since {dayAndTime(status.started_at, now)})</span>],
                    ["Pulse home", <Trunc key="h" text={status.pulse_home} copy />],
                    ["Listening", <span key="p" className="mono">127.0.0.1:{status.port}</span>],
                    ["Sources", <span key="s" className="num">{status.sources.healthy} healthy · {status.sources.degraded} degraded · {status.sources.failed} failed · {status.sources.other} other</span>],
                    ["Runs", <span key="r" className="num">{status.runs.running} running · {status.runs.queued} queued · {status.runs.pending_approval} awaiting approval</span>],
                    ["Definitions", <span key="d" className="num">{count(status.rules)} rules · {count(status.agents)} agents</span>],
                  ]}
                />
              </Section>
              <Section title="Config">
                {errors && errors.length > 0 ? <ConfigErrors errors={errors} /> : null}
                {warnings && warnings.length > 0 ? <ConfigWarnings warnings={warnings} /> : null}
                {!errors?.length && !warnings?.length ? <p className="faint">No problems in the config files.</p> : null}
              </Section>
            </>
          )}
          <Section title="This window">
            <Dl
              items={[
                [
                  "Connection",
                  <span key="c">
                    {connection.kind === "open"
                      ? `Connected since ${dayAndTime(new Date(connection.since).toISOString(), now)}`
                      : connection.kind === "waiting"
                        ? `Reconnecting (attempt ${connection.attempt})`
                        : connection.kind === "unauthorized"
                          ? "Not authorized"
                          : connection.kind}
                  </span>,
                ],
                [
                  "Theme",
                  <span key="t" className="seg" role="radiogroup" aria-label="Theme">
                    {(["system", "light", "dark"] as ThemeChoice[]).map((t) => (
                      <button key={t} type="button" role="radio" aria-checked={theme === t} className={`seg-item${theme === t ? " is-active" : ""}`} onClick={() => setTheme(t)}>
                        {t === "system" ? "Follow the system" : t === "light" ? "Light" : "Dark"}
                      </button>
                    ))}
                  </span>,
                ],
              ]}
            />
          </Section>
          <Section title="Keyboard">
            <ShortcutList />
          </Section>
          {status && (
            <Section title="Stop the daemon">
              <Notice
                tone="neutral"
                title="Shut Pulse down"
                actions={
                  <Button variant="danger" onClick={() => setShutdown(true)}>
                    Shut down…
                  </Button>
                }
              >
                Stops watching, stops running agents (their process groups are terminated) and closes this connection. There is no way to start it again from this window: run <code>pulse start</code> in a terminal, or open the desktop app.
              </Notice>
            </Section>
          )}
        </div>
      </div>
      <Confirm
        open={shutdown}
        onClose={() => setShutdown(false)}
        title="Shut down the daemon?"
        confirm="Shut down"
        danger
        onConfirm={() => void attempt("Shut down", () => client.call("system.shutdown", {}), "The daemon is shutting down")}
      >
        <p>
          Running agents are stopped. To start Pulse again, run <code>pulse start</code>.
        </p>
      </Confirm>
    </div>
  );
}
