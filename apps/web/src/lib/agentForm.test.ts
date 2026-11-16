import { describe, expect, it } from "vitest";

import type { AgentInfo } from "../contracts";
import { agentToForm, emptyAgentForm, formToAgent } from "./agentForm";

const origin = { kind: "user" as const, path: "/home/ada/.pulse/pulse.toml" };

describe("agent form mapping", () => {
  it("round-trips each adapter", () => {
    const agents = [
      { name: "study", adapter: "claude-code", model: "sonnet", permission_mode: "acceptEdits", allowed_tools: ["Read"], extra_args: [], allow: ["fs.*:~/U/**"], cwd: "~/U", timeout: "15m", max_concurrent: 1, max_queued: 8, env: { A: "1" } },
      { name: "fixer", adapter: "codex", sandbox: "workspace-write", extra_args: ["--x"], allow: [], timeout: "20m", max_concurrent: 2, max_queued: 0, env: {} },
      { name: "fmt", adapter: "command", command: ["prettier", "--write", "--", "{{path}}"], stdin: "none", allow: ["fs.*"], timeout: "1m", max_concurrent: 1, max_queued: 8, env: {} },
      { name: "editor", adapter: "mcp", allow: ["git.*"], timeout: "15m", max_concurrent: 1, max_queued: 8, env: {} },
    ] as AgentInfo["config"][];
    for (const config of agents) {
      const { agent, problems } = formToAgent(agentToForm({ config, origin, running: 0, queued: 0 }));
      expect(problems).toEqual([]);
      expect(agent).toEqual(config);
    }
  });

  it("refuses placeholders in the program and reserved environment keys", () => {
    const f = { ...emptyAgentForm(), name: "x", adapter: "command" as const, command: ["{{path}}"], env: [{ key: "PULSE_RUN_ID", value: "1" }] };
    expect(formToAgent(f).problems.map((p) => p.field).sort()).toEqual(["command", "env"]);
  });
});
