import { useSyncExternalStore } from "react";

/** A value with subscribers. Updates replace the value; selectors decide what re-renders. */
export interface Store<T> {
  get(): T;
  set(next: T | ((prev: T) => T)): void;
  subscribe(listener: () => void): () => void;
}

export function createStore<T>(initial: T): Store<T> {
  let value = initial;
  const listeners = new Set<() => void>();
  return {
    get: () => value,
    set(next) {
      const v = typeof next === "function" ? (next as (prev: T) => T)(value) : next;
      if (Object.is(v, value)) return;
      value = v;
      for (const l of listeners) l();
    },
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
}

/** Re-renders only when the selected slice changes (by `Object.is`). */
export function useStore<T, S>(store: Store<T>, select: (value: T) => S): S {
  return useSyncExternalStore(
    store.subscribe,
    () => select(store.get()),
    () => select(store.get()),
  );
}
