import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

// Before anything else: take the token out of the address bar.
import { takeToken } from "./rpc/token";
const token = takeToken();

import { App, NoAccess } from "./App";
import { AppProvider, type App as AppHandle } from "./app/context";
import { Daemon } from "./app/daemon";
import { DeliveryCache } from "./app/deliveries";
import { FeedController } from "./app/feed";
import type { MockControls } from "./mock";
import { RpcClient, type SocketFactory } from "./rpc/client";
import { socketUrl, webSocketFactory } from "./rpc/socket";
import "./styles.css";

async function boot(): Promise<void> {
  const root = createRoot(document.getElementById("root") as HTMLElement);
  let connect: SocketFactory;
  let mock: MockControls | null = null;
  // `import.meta.env.MODE` is a build-time constant, so production builds drop this branch
  // and the mock with it.
  if (import.meta.env.MODE === "mock") {
    const m = await import("./mock");
    const created = m.createMock();
    connect = created.connect;
    mock = created.controls;
  } else {
    if (!token) {
      root.render(<NoAccess />);
      return;
    }
    connect = webSocketFactory(socketUrl(), token);
  }

  const client = new RpcClient({ connect });
  const daemon = new Daemon(client);
  const deliveries = new DeliveryCache(client);
  const feed = new FeedController(client);
  // Row markers only ask about events some rule listens to; runs changing can change them.
  daemon.store.subscribe(() => {
    const rules = daemon.store.get().rules;
    deliveries.setRuleTypes((rules ?? []).filter((r) => !r.config.archived).flatMap((r) => r.config.on));
  });
  daemon.onRunsChanged((runs) => {
    for (const r of runs) deliveries.invalidate(r.event);
  });
  client.start();

  const app: AppHandle = { client, daemon, feed, deliveries, mock };
  root.render(
    <StrictMode>
      <AppProvider app={app}>
        <App />
      </AppProvider>
    </StrictMode>,
  );
}

void boot();
