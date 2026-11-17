import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

// Before anything else: take the login code out of the address bar, and the resume code (a
// reload) out of storage.
import { login, ReloadKeeper, takeLoginCode, takeResume } from "./rpc/login";
const loginCode = takeLoginCode();
const resume = takeResume();

import { App, NoAccess } from "./App";
import { AppProvider, type App as AppHandle } from "./app/context";
import { Daemon } from "./app/daemon";
import { FeedController } from "./app/feed";
import { userHome } from "./app/home";
import { OutcomeStore } from "./app/outcomes";
import type { MockControls } from "./mock";
import { RpcClient, type SocketFactory } from "./rpc/client";
import { daemonPort, requestProof } from "./rpc/proof";
import { socketUrl, webSocketFactory } from "./rpc/socket";
import "./styles.css";

async function boot(): Promise<void> {
  const root = createRoot(document.getElementById("root") as HTMLElement);
  let connect: SocketFactory;
  let mock: MockControls | null = null;
  let keeper: ReloadKeeper | null = null;
  // `import.meta.env.MODE` is a build-time constant, so production builds drop this branch
  // and the mock with it.
  if (import.meta.env.MODE === "mock") {
    const m = await import("./mock");
    const created = m.createMock();
    connect = created.connect;
    mock = created.controls;
  } else {
    const port = daemonPort();
    const access = await login(loginCode, resume, { port });
    if (access.kind === "none") {
      root.render(<NoAccess reason={access.reason} />);
      return;
    }
    const k = new ReloadKeeper(access.token);
    keeper = k;
    connect = webSocketFactory(socketUrl(), access.token, {
      prove: () => requestProof(access.token, { port }),
      onProven: (instance) => k.connected(instance),
    });
  }

  const client = new RpcClient({ connect });
  // A token the daemon refuses (4401) will not start working again; only `pulse open` helps.
  client.onStateChange((s) => {
    if (s.kind === "unauthorized") keeper?.forget();
  });
  const daemon = new Daemon(client);
  daemon.store.subscribe(() => userHome.set(daemon.store.get().status?.user_home ?? null));
  const outcomes = new OutcomeStore((run) => daemon.store.get().runs.get(run)?.status);
  daemon.onRunsChanged((runs) => outcomes.applyRuns(runs));
  const feed = new FeedController(client, outcomes);
  client.start();

  const app: AppHandle = { client, daemon, feed, outcomes, mock };
  root.render(
    <StrictMode>
      <AppProvider app={app}>
        <App />
      </AppProvider>
    </StrictMode>,
  );
}

void boot();
