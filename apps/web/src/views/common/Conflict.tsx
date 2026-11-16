// What an editor shows when the definition it edits changed elsewhere (by hand in the file, from
// the CLI, from another window): what changed, and a deliberate choice. The form is never reset
// unless the user picks "Reload".

import { useState } from "react";

import type { Expected } from "../../contracts";
import { fieldChanges, showValue } from "../../lib/edits";
import { visible } from "../../lib/format";
import { Button, Notice } from "../../ui/primitives";

export interface Versioned<C> {
  config: C;
  revision: string;
}

/** The first defined value `v` had: what an editor was opened on, even after it disappears. */
export function useFirst<T>(v: T | undefined): T | undefined {
  const [first, setFirst] = useState(v);
  if (first === undefined && v !== undefined) {
    setFirst(v);
    return v;
  }
  return first;
}

/**
 * Whether there is a conflict to show: `base` is what the edits are based on (null: creating),
 * `current` the definition as the daemon has it now (null: it does not exist).
 */
export function hasConflict<C>(base: Versioned<C> | null, current: Versioned<C> | null): boolean {
  if (!base) return current !== null;
  return current === null || current.revision !== base.revision;
}

export function ConflictNotice<C extends object>({
  what,
  name,
  base,
  current,
  onReload,
  onOverwrite,
}: {
  what: "rule" | "agent" | "source";
  name: string;
  base: Versioned<C> | null;
  current: Versioned<C> | null;
  /** Take the current definition into the form, discarding the user's edits. */
  onReload: (current: Versioned<C>) => void;
  /** Save the form over whatever is there now, naming exactly that. */
  onOverwrite: (expected: Expected) => void;
}) {
  const shown = visible(name, { singleLine: true });
  if (!base && current) {
    return (
      <Notice
        tone="attention"
        title={`A ${what} named ${shown} already exists`}
        actions={<Button size="sm" variant="attention" onClick={() => onOverwrite({ kind: "revision", revision: current.revision })}>Replace it with mine</Button>}
      >
        It was created elsewhere while you were writing this one. Your form is kept: choose another name, or replace the existing {what} with yours.
      </Notice>
    );
  }
  if (base && !current) {
    return (
      <Notice tone="attention" title={`This ${what} was removed while you were editing it`} actions={<Button size="sm" variant="attention" onClick={() => onOverwrite({ kind: "absent" })}>Create it again with mine</Button>}>
        Someone removed it (from the config file, the CLI or another window). Your form is kept; saving would add it back.
      </Notice>
    );
  }
  if (!base || !current) return null;
  const changes = fieldChanges(base.config, current.config);
  return (
    <Notice
      tone="attention"
      title={`This ${what} changed while you were editing it`}
      actions={
        <>
          <Button size="sm" onClick={() => onReload(current)}>Reload (discard my edits)</Button>
          <Button size="sm" variant="attention" onClick={() => onOverwrite({ kind: "revision", revision: current.revision })}>
            Save mine anyway
          </Button>
        </>
      }
    >
      <p>It was changed elsewhere (in the config file, from the CLI or another window). Your form is kept. Saving yours replaces these changes:</p>
      {changes.length === 0 ? (
        <p className="faint">Only formatting differs in a way that matters to Pulse.</p>
      ) : (
        <ul className="plain conflict-changes">
          {changes.map((c) => (
            <li key={c.field}>
              <code>{c.field}</code>: <span className="mono">{visible(showValue(c.before), { singleLine: true })}</span> → <span className="mono">{visible(showValue(c.after), { singleLine: true })}</span>
            </li>
          ))}
        </ul>
      )}
    </Notice>
  );
}
