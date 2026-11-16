// "Test this rule against a past event": a dry run through the daemon's own routing, with no
// side effects, for a saved rule or an unsaved draft.

import { useEffect, useState } from "react";

import { useApp } from "../../app/context";
import { useBindings } from "../../app/keys";
import type { ConfigError, Event, RuleRef, RuleTestResult } from "../../contracts";
import { deliveryLabel, deliveryTone, outcomeSentence } from "../../lib/explain";
import { clockTime, dayAndTime, shortId, sourceName } from "../../lib/format";
import { typeGlobMatches } from "../../lib/syntax";
import { RpcCallError, ErrorCodes } from "../../rpc/client";
import { Icon } from "../../ui/icons";
import { Button, Input, Notice, Pill, Toggle, Trunc, cx } from "../../ui/primitives";
import { PromptView } from "../common/PromptView";

interface Outcome {
  result: RuleTestResult;
  event: Event;
}

export function RuleTester({
  makeRef,
  on,
  initialEvent,
  defaultIgnoreState,
  onConfigErrors,
}: {
  /** The rule to test, or a reason it cannot be tested yet. */
  makeRef: () => RuleRef | string;
  on: string[];
  initialEvent: string | null;
  defaultIgnoreState: boolean;
  onConfigErrors?: (errors: ConfigError[]) => void;
}) {
  const { client } = useApp();
  const [events, setEvents] = useState<Event[]>([]);
  const [eventId, setEventId] = useState<string | null>(initialEvent);
  const [search, setSearch] = useState("");
  const [ignoreState, setIgnoreState] = useState(defaultIgnoreState);
  const [busy, setBusy] = useState(false);
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  const [error, setError] = useState<string | null>(null);
  const onKey = on.join(",");

  // Candidate events: the newest that match the rule's types (or anything, before `on` is set).
  useEffect(() => {
    let live = true;
    const t = setTimeout(() => {
      client
        .call("events.query", {
          filter: { types: on.filter(Boolean), sources: [], ...(search.trim() ? { text: search.trim() } : {}) },
          order: "newest_first",
          limit: 40,
        })
        .then((p) => live && setEvents(p.events))
        .catch(() => live && setEvents([]));
    }, 200);
    return () => {
      live = false;
      clearTimeout(t);
    };
    // `onKey` stands for `on`.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, onKey, search]);

  // An event chosen from elsewhere ("Test" on a delivery) may not be in the list: fetch it.
  const [pinned, setPinned] = useState<Event | null>(null);
  useEffect(() => {
    if (!initialEvent) return;
    let live = true;
    client.call("events.get", { id: initialEvent }).then(
      (d) => live && setPinned(d.event),
      () => {},
    );
    return () => {
      live = false;
    };
  }, [client, initialEvent]);

  const list = pinned && !events.some((e) => e.id === pinned.id) ? [pinned, ...events] : events;
  const selected = list.find((e) => e.id === eventId) ?? null;

  const run = async () => {
    const ref = makeRef();
    if (typeof ref === "string") {
      setError(ref);
      return;
    }
    if (!selected) {
      setError("Pick an event to test against.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      // For a debounced rule the daemon also says what fires once the debounce elapses
      // (`after_debounce`), with the prompt it would send.
      const result = await client.call("rules.test", { rule: ref, event: selected.id, ignore_runtime_state: ignoreState });
      setOutcome({ result, event: selected });
      onConfigErrors?.([]);
    } catch (err) {
      if (err instanceof RpcCallError && err.code === ErrorCodes.ConfigInvalid) {
        const errors = (err.data as { errors?: ConfigError[] } | undefined)?.errors ?? [];
        onConfigErrors?.(errors);
        setError(`The draft does not validate: ${errors.map((e) => `${e.field ? `${e.field}: ` : ""}${e.message}`).join("; ") || err.message}`);
      } else {
        setError(err instanceof Error ? err.message : String(err));
      }
      setOutcome(null);
    }
    setBusy(false);
  };



  useBindings([{ keys: "mod+enter", label: "Test against the selected event", group: "Rules", run: () => void run() }]);

  return (
    <div className="tester">
      <div className="tester-head">
        <h3>
          <Icon name="flask" size={14} /> Test against a past event
        </h3>
        <p className="faint">A dry run through the daemon's routing. Nothing is recorded and nothing runs.</p>
      </div>
      <div className="tester-pick">
        <Input value={search} onChange={(e) => setSearch(e.target.value)} placeholder={on.length ? `Search recent ${on.join(", ")} events` : "Search recent events"} aria-label="Search events" />
        <ul className="pick-events" role="listbox" aria-label="Events">
          {list.length === 0 && <li className="faint pick-empty">No recent events of {on.length ? on.join(", ") : "any type"}.</li>}
          {list.map((e) => (
            <li
              key={e.id}
              role="option"
              aria-selected={e.id === eventId}
              tabIndex={0}
              className={cx("pick-event", e.id === eventId && "is-selected")}
              onClick={() => setEventId(e.id)}
              onKeyDown={(ev) => {
                if ((ev.key === "Enter" || ev.key === " ") && !ev.ctrlKey && !ev.metaKey) {
                  ev.preventDefault();
                  setEventId(e.id);
                }
              }}
            >
              <span className="num faint">{clockTime(e.time)}</span>
              <span className="mono">{e.type}</span>
              <span className="pick-subject">{e.subject ? <Trunc text={e.subject} /> : null}</span>
            </li>
          ))}
        </ul>
      </div>
      <div className="tester-run">
        <Toggle checked={ignoreState} onChange={setIgnoreState} label="Ignore runtime state" hint="As if the rule had never fired: no snooze, cooldown, rate limit or pending debounce." />
        <Button variant="primary" icon="flask" onClick={() => void run()} disabled={busy || !selected} shortcut="mod+enter">
          {busy ? "Testing…" : "Test"}
        </Button>
      </div>
      {error && (
        <Notice tone="error" title="Cannot test">
          {error}
        </Notice>
      )}
      {outcome && <TestResult outcome={outcome} on={on} />}
    </div>
  );
}

function TestResult({ outcome, on }: { outcome: Outcome; on: string[] }) {
  const { result, event } = outcome;
  const afterDebounce = result.after_debounce ? { decision: result.after_debounce } : null;
  const when = (iso: string) => dayAndTime(iso, Date.now());
  const head = (
    <p className="faint test-against">
      Against <span className="mono">{event.type}</span> {shortId(event.id)} from {sourceName(event.source)} at <span className="num">{dayAndTime(event.time, Date.now())}</span>
    </p>
  );
  if (!result.matched || !result.decision) {
    const typeOk = on.some((g) => typeGlobMatches(g, event.type));
    return (
      <div className="test-result">
        {head}
        <Notice tone="neutral" title="Does not match">
          {typeOk ? (
            <>The event's type matches, but the <code>where</code> table does not. No delivery would be recorded.</>
          ) : (
            <>
              The event's type <code>{event.type}</code> is not covered by <code>on</code>.
            </>
          )}
        </Notice>
      </div>
    );
  }
  const d = result.decision;
  const tone = deliveryTone(d.outcome);
  const prompt = d.outcome.kind === "fire" || d.outcome.kind === "await_approval" ? d.outcome.prompt : null;
  return (
    <div className="test-result">
      {head}
      <div className={cx("decision", `tone-${tone}`)}>
        <Pill tone={tone} {...(tone === "loop" ? { icon: "loop" as const } : {})}>
          {deliveryLabel(d.outcome)}
        </Pill>
        <span>{outcomeSentence(d.outcome, d.agent, when)}</span>
        <span className="faint num">chain depth {d.chain_depth}</span>
      </div>
      {prompt && <PromptView text={prompt.text} delimiter={prompt.delimiter} truncated={prompt.truncated} missing={prompt.missing} />}
      {(d.outcome.kind === "fire" || d.outcome.kind === "await_approval") && !prompt && <p className="faint">No prompt: the agent is a command and the rule has none.</p>}
      {afterDebounce?.decision && (
        <div className="after-debounce">
          <h4 className="sub-head">When the debounce elapses</h4>
          <div className={cx("decision", `tone-${deliveryTone(afterDebounce.decision.outcome)}`)}>
            <Pill tone={deliveryTone(afterDebounce.decision.outcome)}>{deliveryLabel(afterDebounce.decision.outcome)}</Pill>
            <span>{outcomeSentence(afterDebounce.decision.outcome, afterDebounce.decision.agent, when)}</span>
          </div>
          {(afterDebounce.decision.outcome.kind === "fire" || afterDebounce.decision.outcome.kind === "await_approval") && afterDebounce.decision.outcome.prompt && (
            <PromptView
              text={afterDebounce.decision.outcome.prompt.text}
              delimiter={afterDebounce.decision.outcome.prompt.delimiter}
              truncated={afterDebounce.decision.outcome.prompt.truncated}
              missing={afterDebounce.decision.outcome.prompt.missing}
            />
          )}
        </div>
      )}
    </div>
  );
}
