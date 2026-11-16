// The agent editor's form and its mapping to `AgentConfig` (a union on `adapter`).

import type { AgentConfig, AgentInfo, StdinMode } from "../contracts";
import { nameProblem, parseDuration, parseScope } from "./syntax";

export type Adapter = AgentConfig["adapter"];
export const ADAPTERS: Adapter[] = ["claude-code", "codex", "command", "mcp"];

export interface AgentForm {
  name: string;
  project: string | null;
  adapter: Adapter;
  program: string;
  model: string;
  permissionMode: string;
  allowedTools: string[];
  sandbox: string;
  extraArgs: string[];
  command: string[];
  stdin: StdinMode;
  allow: string[];
  cwd: string;
  timeout: string;
  maxConcurrent: string;
  maxQueued: string;
  env: { key: string; value: string }[];
}

export function emptyAgentForm(): AgentForm {
  return {
    name: "",
    project: null,
    adapter: "claude-code",
    program: "",
    model: "",
    permissionMode: "",
    allowedTools: [],
    sandbox: "",
    extraArgs: [],
    command: [],
    stdin: "prompt",
    allow: [],
    cwd: "",
    timeout: "15m",
    maxConcurrent: "1",
    maxQueued: "8",
    env: [],
  };
}

export function agentToForm(info: AgentInfo): AgentForm {
  const a = info.config;
  const project = info.origin.kind === "project" ? info.origin.name : null;
  const f = emptyAgentForm();
  f.name = project && a.name.startsWith(`${project}/`) ? a.name.slice(project.length + 1) : a.name;
  f.project = project;
  f.adapter = a.adapter;
  // Lists and maps are left off the wire when empty.
  f.allow = [...(a.allow ?? [])];
  f.cwd = a.cwd ?? "";
  f.timeout = a.timeout ?? "15m";
  f.maxConcurrent = String(a.max_concurrent ?? 1);
  f.maxQueued = String(a.max_queued ?? 8);
  f.env = Object.entries(a.env ?? {}).map(([key, value]) => ({ key, value }));
  switch (a.adapter) {
    case "claude-code":
      f.program = a.program ?? "";
      f.model = a.model ?? "";
      f.permissionMode = a.permission_mode ?? "";
      f.allowedTools = [...(a.allowed_tools ?? [])];
      f.extraArgs = [...(a.extra_args ?? [])];
      break;
    case "codex":
      f.program = a.program ?? "";
      f.model = a.model ?? "";
      f.sandbox = a.sandbox ?? "";
      f.extraArgs = [...(a.extra_args ?? [])];
      break;
    case "command":
      f.command = [...(a.command ?? [])];
      f.stdin = a.stdin ?? "prompt";
      break;
    case "mcp":
      break;
  }
  return f;
}

export function formToAgent(f: AgentForm): { agent: AgentConfig; problems: { field: string; message: string }[] } {
  const problems: { field: string; message: string }[] = [];
  const np = nameProblem(f.name.trim());
  if (np) problems.push({ field: "name", message: np });
  f.allow.forEach((s, i) => {
    const p = parseScope(s).problem;
    if (p) problems.push({ field: "allow", message: `${s}: ${p}` });
    void i;
  });
  if (parseDuration(f.timeout) === null) problems.push({ field: "timeout", message: "Use a duration like 15m." });
  const mc = Number(f.maxConcurrent);
  const mq = Number(f.maxQueued);
  if (!Number.isInteger(mc) || mc < 1) problems.push({ field: "max_concurrent", message: "A whole number, at least 1." });
  if (!Number.isInteger(mq) || mq < 0) problems.push({ field: "max_queued", message: "A whole number, 0 or more." });
  const env: Record<string, string> = {};
  for (const { key, value } of f.env) {
    const k = key.trim();
    if (!k) continue;
    if (k.startsWith("PULSE_")) problems.push({ field: "env", message: `${k}: keys starting with PULSE_ are reserved.` });
    env[k] = value;
  }
  const common = {
    name: f.name.trim(),
    allow: f.allow,
    timeout: f.timeout.trim(),
    max_concurrent: mc,
    max_queued: mq,
    env,
    ...(f.cwd.trim() ? { cwd: f.cwd.trim() } : {}),
  };
  const opt = (k: string, v: string) => (v.trim() ? { [k]: v.trim() } : {});
  let agent: AgentConfig;
  switch (f.adapter) {
    case "claude-code":
      agent = { ...common, adapter: "claude-code", allowed_tools: f.allowedTools, extra_args: f.extraArgs, ...opt("program", f.program), ...opt("model", f.model), ...opt("permission_mode", f.permissionMode) };
      break;
    case "codex":
      agent = { ...common, adapter: "codex", extra_args: f.extraArgs, ...opt("program", f.program), ...opt("model", f.model), ...opt("sandbox", f.sandbox) };
      break;
    case "command":
      if (f.command.length === 0) problems.push({ field: "command", message: "Give the program and its arguments, one per entry." });
      else if (f.command[0]?.includes("{{")) problems.push({ field: "command", message: "The program (first entry) cannot contain placeholders." });
      agent = { ...common, adapter: "command", command: f.command, stdin: f.stdin };
      break;
    case "mcp":
      agent = { ...common, adapter: "mcp" };
      break;
  }
  return { agent, problems };
}
