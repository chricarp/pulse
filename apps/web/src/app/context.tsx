import { createContext, useContext, type ReactNode } from "react";

import type { MockControls } from "../mock";
import type { RpcClient } from "../rpc/client";
import { useStore } from "../store/store";
import type { Daemon, DaemonState } from "./daemon";
import type { DeliveryCache } from "./deliveries";
import type { FeedController } from "./feed";

export interface App {
  client: RpcClient;
  daemon: Daemon;
  feed: FeedController;
  deliveries: DeliveryCache;
  /** Present only in `--mode mock` builds. */
  mock: MockControls | null;
}

const Ctx = createContext<App | null>(null);

export function AppProvider({ app, children }: { app: App; children: ReactNode }) {
  return <Ctx.Provider value={app}>{children}</Ctx.Provider>;
}

export function useApp(): App {
  const app = useContext(Ctx);
  if (!app) throw new Error("useApp outside AppProvider");
  return app;
}

export function useDaemon<S>(select: (s: DaemonState) => S): S {
  return useStore(useApp().daemon.store, select);
}
