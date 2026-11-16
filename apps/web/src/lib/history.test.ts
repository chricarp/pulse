import { describe, expect, it } from "vitest";

import type { Delivery } from "../contracts";
import { mergeDeliveries } from "./history";

const d = (id: string): Delivery => ({ id, event: `e${id}`, rule: "r", agent: "a", at: "", outcome: { kind: "fired" }, run: null });

describe("a rule's history", () => {
  it("keeps the pages loaded with more when newer deliveries arrive, in any order", () => {
    const loaded = [d("05"), d("04"), d("03"), d("02"), d("01")];
    const head = [d("07"), d("06"), d("05"), d("04")];
    expect(mergeDeliveries(loaded, head).map((x) => x.id)).toEqual(["07", "06", "05", "04", "03", "02", "01"]);
    expect(mergeDeliveries(head, loaded)).toEqual(mergeDeliveries(loaded, head));
  });
});
