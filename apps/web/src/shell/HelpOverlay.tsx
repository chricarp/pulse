import { useSyncExternalStore } from "react";

import { allBindings, onBindingsChanged, type Binding } from "../app/keys";
import { Kbd, Modal, Button } from "../ui/primitives";

let snapshot: Binding[] = [];
let stale = true;
function subscribe(l: () => void): () => void {
  return onBindingsChanged(() => {
    stale = true;
    l();
  });
}
function get(): Binding[] {
  if (stale) {
    snapshot = allBindings();
    stale = false;
  }
  return snapshot;
}

export function useAllBindings(): Binding[] {
  return useSyncExternalStore(subscribe, get);
}

export function ShortcutList() {
  const bindings = useAllBindings();
  const groups = new Map<string, Binding[]>();
  for (const b of bindings) {
    const list = groups.get(b.group) ?? [];
    list.push(b);
    groups.set(b.group, list);
  }
  return (
    <div className="shortcuts">
      {[...groups].map(([group, list]) => (
        <section key={group}>
          <h4>{group}</h4>
          <dl>
            {list.map((b) => (
              <div key={b.keys} className="shortcut">
                <dt>{b.label}</dt>
                <dd>
                  <Kbd keys={b.keys} />
                </dd>
              </div>
            ))}
          </dl>
        </section>
      ))}
    </div>
  );
}

export function HelpOverlay({ open, onClose }: { open: boolean; onClose: () => void }) {
  return (
    <Modal open={open} onClose={onClose} title="Keyboard shortcuts" wide actions={<Button onClick={onClose}>Close</Button>}>
      <p className="muted">Shortcuts for the view you are in are listed with the global ones. Two-key shortcuts like G F are typed one after the other.</p>
      <ShortcutList />
    </Modal>
  );
}
