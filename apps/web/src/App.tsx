import { useDaemon } from "./app/context";
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
  if (connection.kind === "unauthorized") return <NoAccess reason={connection.reason} />;
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

export function NoAccess({ reason }: { reason?: string }) {
  return (
    <div className="no-access">
      <Empty title="This window cannot talk to the daemon" tone="error">
        <p>{reason ? `${reason} ` : ""}Pulse needs the daemon's token, which it hands to the window when you open it with:</p>
        <pre className="code">pulse open</pre>
        <p className="faint">The token is passed in the address fragment, read once, and removed from the address bar. If you opened this page by typing its address, run the command above instead.</p>
      </Empty>
    </div>
  );
}
