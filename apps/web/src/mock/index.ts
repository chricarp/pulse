// Development and test entry for the mock daemon. Imported only when the app is built with
// `--mode mock`; production builds never contain it.

import { MockDaemon } from "./daemon";
import { mockSocketFactory } from "./transport";

export interface MockControls {
  storm(total?: number, seconds?: number): void;
  bigRun(bytes?: number): string;
  requestApproval(): void;
  dropConnection(): void;
  stopDaemon(): void;
  startDaemon(): void;
  changeProjectFile(name: string): void;
  editByHand(kind: "rule" | "agent" | "source", name: string, patch: Record<string, unknown>): void;
}

export function createMock(): { connect: ReturnType<typeof mockSocketFactory>; controls: MockControls } {
  const params = new URLSearchParams(window.location.search);
  const daemon = new MockDaemon({ trickle: params.get("trickle") !== "0" });
  const controls: MockControls = {
    storm: (total, seconds) => daemon.storm(total, seconds),
    bigRun: (bytes) => daemon.bigRun(bytes),
    requestApproval: () => daemon.requestApproval(),
    dropConnection: () => daemon.dropConnections(),
    stopDaemon: () => daemon.stop(),
    startDaemon: () => daemon.restart(),
    changeProjectFile: (name) => daemon.changeProjectFile(name),
    editByHand: (kind, name, patch) => daemon.editByHand(kind, name, patch),
  };
  (window as unknown as { __pulseMock: MockControls }).__pulseMock = controls;
  return { connect: mockSocketFactory(daemon), controls };
}
