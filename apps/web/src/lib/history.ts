// A rule's history as the UI holds it: pages loaded so far, newest first, with newer deliveries
// merged in as they are recorded. Deliveries are only ever added and their ids sort by time, so
// merging by id is right whatever order pages and refreshes arrive in.

import type { Delivery } from "../contracts";

export function mergeDeliveries(held: readonly Delivery[], incoming: readonly Delivery[]): Delivery[] {
  if (incoming.length === 0) return [...held];
  const byId = new Map(held.map((d) => [d.id, d]));
  for (const d of incoming) byId.set(d.id, d);
  return [...byId.values()].sort((a, b) => (a.id < b.id ? 1 : a.id > b.id ? -1 : 0));
}
