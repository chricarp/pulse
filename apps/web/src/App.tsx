import { useDaemon } from "./app/context";
import type { NoAccessReason } from "./rpc/login";
import { useLocation, useRoute } from "./app/router";
import { Shell } from "./shell/Shell";
import { ErrorBoundary } from "./ui/ErrorBoundary";
import { Empty } from "./ui/primitives";
import { FeedView } from "./views/feed/FeedView";
import { RunsView } from "./views/runs/RunsView";
import { RulesView } from "./views/rules/RulesView";
import { AgentsView } from "./views/agents/AgentsView";
import { SourcesView } from "./views/sources/SourcesView";
import { ProjectsView } from "./views/projects/ProjectsView";
import { SystemView } from "./views/system/SystemView";

export function App() {
  const connection = useDaemon((s) => s.connection);
  if (connection.kind === "unauthorized") return <NoAccess reason="token-rejected" />;
  return (
    <Shell>
      <Bounded />
    </Shell>
  );
}

function Bounded() {
  const { path } = useLocation();
  return (
    <ErrorBoundary resetKey={path}>
      <View />
    </ErrorBoundary>
  );
}

function View() {
  const route = useRoute();
  switch (route.view) {
    case "feed":
      return <FeedView eventId={route.eventId} />;
    case "runs":
      return <RunsView runId={route.runId} />;
    case "rules":
      return <RulesView name={route.name} mode={route.mode} />;
    case "agents":
      return <AgentsView name={route.name} mode={route.mode} />;
    case "sources":
      return <SourcesView name={route.name} mode={route.mode} />;
    case "projects":
      return <ProjectsView />;
    case "system":
      return <SystemView />;
  }
}

const NO_ACCESS: Record<NoAccessReason, { title: string; body: string }> = {
  "no-code": {
    title: "Open Pulse from the command line",
    body: "This window has no access to the daemon yet. Pulse gives a window a one-time login code when you open it with the command below. If you typed this address or used a bookmark, run it instead.",
  },
  "code-refused": {
    title: "This login link has expired",
    body: "Login links work once, for two minutes. Run the command below for a new one.",
  },
  unreachable: {
    title: "The daemon did not answer",
    body: "This page could not reach the daemon to log in. Check that it is running (pulse status, or pulse start), then open the UI again.",
  },
  "token-rejected": {
    title: "The daemon no longer accepts this window",
    body: "Its token changed (it was rotated, or another Pulse home is running on this port). This window stopped trying to connect. Log in again with the command below.",
  },
};

export function NoAccess({ reason }: { reason: NoAccessReason }) {
  const text = NO_ACCESS[reason];
  return (
    <div className="no-access" data-reason={reason}>
      <Empty title={text.title} tone="error">
        <p>{text.body}</p>
        <pre className="code">pulse open</pre>
        <p className="faint">The command opens a link with a one-time code. The page exchanges it for access and removes it from the address bar; a reload of this tab keeps working, a new tab needs the command again.</p>
      </Empty>
    </div>
  );
}
