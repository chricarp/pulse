// Inventories the licenses of everything this checkout builds with, for one exact build:
// the Rust dependency tree (from `cargo metadata`) and the installed npm tree (from
// node_modules/.pnpm). Writes inventory.json and THIRD_PARTY_NOTICES.txt to the directory given
// as its argument, and exits 1 when a dependency declares no license or a license outside the
// policy below. Run it after `pnpm install --frozen-lockfile`, from the repository root:
//
//   pnpm run licenses:report /tmp/pulse-licenses [--target <rust target triple>]
//
// No network beyond what cargo itself needs to read the locked crates, no registry lookups, no
// tool to install. It is evidence for a person reviewing an artifact, not a compatibility verdict.
// It does not cover the Electron and Chromium binaries' own notices, which ship inside the
// Electron distribution (LICENSE and LICENSES.chromium.html next to the executable).

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, lstatSync, mkdirSync, readFileSync, readdirSync, realpathSync, writeFileSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// Licenses accepted without review: permissive, and present in the tree when this was written.
// Several still require their notice to travel with a binary (MIT, BSD, ISC, Apache-2.0,
// Unicode-3.0, CDLA-Permissive-2.0, ...): THIRD_PARTY_NOTICES.txt collects those texts.
const ALLOWED = new Set([
  "MIT",
  "Apache-2.0",
  "ISC",
  "BSD-2-Clause",
  "BSD-3-Clause",
  "0BSD",
  "Zlib",
  "Unlicense",
  "CC0-1.0",
  "BSL-1.0",
  "Unicode-3.0",
  "BlueOak-1.0.0",
  // Mozilla's CA certificate data in webpki-roots / webpki-root-certs (calendar URLs over TLS).
  "CDLA-Permissive-2.0",
]);
const ALLOWED_EXCEPTIONS = new Set(["LLVM-exception"]);

// Reviewed one by one: copyleft or unusual, accepted only for build and development tools that do
// not end up in the binary, the web UI or the desktop app. A package listed here that becomes
// shipped, or whose declared license changes, fails the report again.
const REVIEWED = [
  {
    name: /^lightningcss(-[a-z0-9-]+)?$/,
    license: "MPL-2.0",
    reason: "Vite's CSS transformer and minifier, run at build time; the CSS it emits is the project's own",
  },
  {
    name: /^@resvg\/resvg-js(-[a-z0-9-]+)?$/,
    license: "MPL-2.0",
    reason: "renders assets/icon.svg to PNG when the desktop app is built; the PNGs are the project's icon, the library is not shipped",
  },
  {
    name: /^truncate-utf8-bytes$/,
    license: "WTFPL",
    reason: "a dependency of electron-builder (packaging tool), not shipped",
  },
  {
    name: /^argparse$/,
    license: "Python-2.0",
    reason: "a dependency of js-yaml in the build and test tooling, not shipped",
  },
];

const NOTICE = /^(?:(?:third|3rd)[-_ ]?party[-_ ]?)?(?:licen[cs]es?|copying|notices?(?:text)?|copyright(?:notice)?s?|authors)(?:[._-].*)?$/i;

function usage(message) {
  console.error(`${message}\nusage: pnpm run licenses:report <output directory outside the repository> [--target <triple>]`);
  process.exit(2);
}

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
let target = null;
let destination = null;
for (let i = 0; i < args.length; i++) {
  if (args[i] === "--target") target = args[++i] ?? usage("--target needs a value");
  else if (destination === null) destination = args[i];
  else usage(`unexpected argument: ${args[i]}`);
}
if (!destination) usage("missing output directory");
const output = resolve(destination);
if (output === root || !relative(root, output).startsWith("..")) usage("keep generated license reports outside the repository");

const sha256 = (data) => createHash("sha256").update(data).digest("hex");
const run = (cmd, argv) => execFileSync(cmd, argv, { cwd: root, encoding: "utf8", maxBuffer: 256 * 1024 * 1024 });

/** Parses an SPDX expression (and the legacy `A/B` form) into a tree; null when unparseable. */
function parse(expression) {
  const tokens = expression.replace(/\//g, " OR ").match(/\(|\)|[^\s()]+/g) ?? [];
  let i = 0;
  const peek = () => tokens[i];
  function primary() {
    const t = tokens[i++];
    if (t === "(") {
      const node = or();
      if (tokens[i++] !== ")") throw new Error("unbalanced");
      return node;
    }
    if (t === undefined || ["AND", "OR", "WITH", ")"].includes(t)) throw new Error("expected a license");
    let node = { id: t.replace(/\+$/, "") };
    if (peek() === "WITH") {
      i++;
      node = { with: node, exception: tokens[i++] };
    }
    return node;
  }
  function and() {
    let node = primary();
    while (peek() === "AND") {
      i++;
      node = { and: [node, primary()] };
    }
    return node;
  }
  function or() {
    let node = and();
    while (peek() === "OR") {
      i++;
      node = { or: [node, and()] };
    }
    return node;
  }
  try {
    const tree = or();
    return i === tokens.length ? tree : null;
  } catch {
    return null;
  }
}

/** Whether some choice the expression offers is made only of allowed licenses. */
function allowed(node) {
  if (node.or) return node.or.some(allowed);
  if (node.and) return node.and.every(allowed);
  if (node.with) return allowed(node.with) && ALLOWED_EXCEPTIONS.has(node.exception);
  return ALLOWED.has(node.id);
}

function notices(directory) {
  if (!existsSync(directory)) return [];
  return readdirSync(directory, { withFileTypes: true })
    .filter((e) => e.isFile() && NOTICE.test(e.name))
    .map((e) => {
      const text = readFileSync(join(directory, e.name), "utf8");
      return { file: e.name, text, sha256: sha256(text) };
    })
    .sort((a, b) => a.file.localeCompare(b.file, "en"));
}

function judge(entry) {
  if (!entry.license) return { status: "problem", why: "declares no license" };
  const tree = parse(entry.license);
  if (tree === null) return { status: "problem", why: `license expression cannot be read: ${entry.license}` };
  if (allowed(tree)) return { status: "allowed" };
  const review = REVIEWED.find((r) => r.name.test(entry.name) && r.license === entry.license);
  if (review && !entry.shipped) return { status: "reviewed", why: review.reason };
  if (review) return { status: "problem", why: `${entry.license} was reviewed only as a build tool, and this is now shipped` };
  return { status: "problem", why: `${entry.license} is outside the policy in scripts/license-report.mjs` };
}

// ---- Rust ---------------------------------------------------------------------------------

if (target === null) target = /^host: (\S+)$/m.exec(run("rustc", ["-vV"]))?.[1] ?? usage("cannot tell the host target; pass --target");
const metadata = JSON.parse(run("cargo", ["metadata", "--format-version", "1", "--locked", "--filter-platform", target]));
const byId = new Map(metadata.packages.map((p) => [p.id, p]));
const nodes = new Map(metadata.resolve.nodes.map((n) => [n.id, n]));
const workspace = new Set(metadata.workspace_members);
const isProcMacro = (p) => p.targets.some((t) => t.kind.includes("proc-macro"));

// Linked into the `pulse` binary: reachable from pulse-cli through normal dependencies, not
// through build scripts, dev-dependencies or procedural macros (those run in the compiler).
const linked = new Set();
const cli = metadata.packages.find((p) => workspace.has(p.id) && p.name === "pulse-cli");
if (!cli) usage("pulse-cli is not in the workspace");
const queue = [cli.id];
while (queue.length > 0) {
  const id = queue.pop();
  if (linked.has(id)) continue;
  linked.add(id);
  for (const dep of nodes.get(id)?.deps ?? []) {
    if (!dep.dep_kinds.some((k) => k.kind === null)) continue;
    if (isProcMacro(byId.get(dep.pkg))) continue;
    queue.push(dep.pkg);
  }
}

const crates = metadata.packages
  .filter((p) => !workspace.has(p.id))
  .map((p) => {
    const entry = {
      ecosystem: "cargo",
      name: p.name,
      version: p.version,
      license: p.license ?? null,
      licenseFile: p.license_file ?? null,
      repository: p.repository ?? null,
      shipped: linked.has(p.id) ? "linked into the pulse binary" : null,
      notices: notices(dirname(p.manifest_path)),
    };
    return { ...entry, ...judge(entry) };
  });

// ---- npm ----------------------------------------------------------------------------------

const store = join(root, "node_modules", ".pnpm");
if (!existsSync(store)) usage("node_modules/.pnpm is missing: run `pnpm install --frozen-lockfile` first");

function readManifest(directory) {
  try {
    return JSON.parse(readFileSync(join(directory, "package.json"), "utf8"));
  } catch {
    return null;
  }
}

function declaredLicense(manifest) {
  const l = manifest.license ?? manifest.licenses;
  if (typeof l === "string") return l;
  if (Array.isArray(l)) return l.map((x) => (typeof x === "string" ? x : x?.type)).filter(Boolean).join(" OR ") || null;
  if (l && typeof l === "object" && typeof l.type === "string") return l.type;
  return null;
}

// Every installed package directory: node_modules/.pnpm/<key>/node_modules/<name or @scope/name>.
const installed = new Map();
for (const key of readdirSync(store)) {
  const modules = join(store, key, "node_modules");
  if (!existsSync(modules) || !lstatSync(modules).isDirectory()) continue;
  for (const name of readdirSync(modules)) {
    const candidates = name.startsWith("@") ? readdirSync(join(modules, name)).map((n) => join(modules, name, n)) : [join(modules, name)];
    for (const directory of candidates) {
      if (lstatSync(directory).isSymbolicLink()) continue;
      const manifest = readManifest(directory);
      if (manifest?.name && manifest.version) installed.set(directory, manifest);
    }
  }
}

// What the products carry: the web UI's runtime dependencies (bundled into the UI, which the
// binary embeds) and Electron (the desktop app's runtime). The desktop bundles no other npm code.
const shipped = new Map();
function ship(directory, why) {
  const real = realpathSync(directory);
  if (shipped.has(real)) return;
  shipped.set(real, why);
  const manifest = installed.get(real);
  if (!manifest) return;
  const siblings = dirname(manifest.name.includes("/") ? dirname(real) : real);
  for (const dep of Object.keys({ ...manifest.dependencies, ...manifest.optionalDependencies, ...manifest.peerDependencies })) {
    const next = join(siblings, dep);
    if (existsSync(next)) ship(next, why);
  }
}
for (const dep of Object.keys(readManifest(join(root, "apps", "web"))?.dependencies ?? {})) {
  ship(join(root, "apps", "web", "node_modules", dep), "bundled into the web UI (embedded in the pulse binary)");
}
const electron = join(root, "apps", "desktop", "node_modules", "electron");
if (existsSync(electron)) shipped.set(realpathSync(electron), "the desktop app's runtime (its binaries carry their own notices)");

const seen = new Set();
const packages = [];
for (const [directory, manifest] of installed) {
  const id = `${manifest.name}@${manifest.version}`;
  if (seen.has(id)) continue;
  seen.add(id);
  const entry = {
    ecosystem: "npm",
    name: manifest.name,
    version: manifest.version,
    license: declaredLicense(manifest),
    repository: typeof manifest.repository === "string" ? manifest.repository : (manifest.repository?.url ?? null),
    shipped: shipped.get(directory) ?? null,
    notices: notices(directory),
  };
  packages.push({ ...entry, ...judge(entry) });
}

// ---- Report -------------------------------------------------------------------------------

const all = [...crates, ...packages].sort((a, b) =>
  `${a.ecosystem} ${a.name}@${a.version}`.localeCompare(`${b.ecosystem} ${b.name}@${b.version}`, "en"),
);
let commit = null;
let dirty = null;
try {
  commit = run("git", ["rev-parse", "HEAD"]).trim();
  dirty = run("git", ["status", "--porcelain"]).trim() !== "";
} catch {
  // Not a git checkout: the lockfile hashes still identify the tree.
}

const report = {
  scope:
    "Rust crates resolved for one target (all kinds of dependency) and the npm packages installed on this machine (development tooling included). `shipped` names what reaches a product. Not covered: the Electron and Chromium binaries' own notices (inside the Electron distribution), npm platform variants not installed here, other Rust targets, and the operating system. Metadata is not a compatibility verdict.",
  commit,
  dirty,
  rust: { target, lockfileSha256: sha256(readFileSync(join(root, "Cargo.lock"))) },
  npm: { platform: process.platform, arch: process.arch, lockfileSha256: sha256(readFileSync(join(root, "pnpm-lock.yaml"))) },
  policy: { allowed: [...ALLOWED], exceptions: [...ALLOWED_EXCEPTIONS], reviewed: REVIEWED.map((r) => ({ name: String(r.name), license: r.license, reason: r.reason })) },
  problems: all.filter((p) => p.status === "problem").map((p) => ({ ecosystem: p.ecosystem, package: `${p.name}@${p.version}`, license: p.license, why: p.why })),
  reviewed: all.filter((p) => p.status === "reviewed").map((p) => ({ ecosystem: p.ecosystem, package: `${p.name}@${p.version}`, license: p.license, why: p.why })),
  missingNotices: all.filter((p) => p.notices.length === 0).map((p) => `${p.ecosystem} ${p.name}@${p.version}`),
  packages: all.map(({ notices: texts, status, why, ...rest }) => ({
    ...rest,
    status,
    notices: texts.map(({ file, sha256: hash }) => ({ file, sha256: hash })),
  })),
};

mkdirSync(output, { recursive: true });
writeFileSync(join(output, "inventory.json"), `${JSON.stringify(report, null, 2)}\n`);

const section = (title, list) => [
  `\n${"#".repeat(72)}\n# ${title} (${list.length})\n${"#".repeat(72)}`,
  ...list.map(
    (p) =>
      `\n${"=".repeat(72)}\n${p.name}@${p.version} (${p.ecosystem})\nDeclared license: ${p.license ?? "none"}\n` +
      (p.notices.length
        ? p.notices.map((n) => `\n--- ${n.file} ---\n${n.text}`).join("\n")
        : "No license or notice file in this package; obtain the upstream notice before distributing it.\n"),
  ),
];
writeFileSync(
  join(output, "THIRD_PARTY_NOTICES.txt"),
  [
    "Third-party notices for Lictory Pulse",
    report.scope,
    `Commit: ${commit ?? "unknown"}${dirty ? " (with uncommitted changes)" : ""}`,
    `Rust target: ${target}; Cargo.lock SHA-256: ${report.rust.lockfileSha256}`,
    `npm: ${process.platform}/${process.arch}; pnpm-lock.yaml SHA-256: ${report.npm.lockfileSha256}`,
    ...section("Linked into the pulse binary", crates.filter((p) => p.shipped)),
    ...section("Bundled into the web UI, and the desktop app's runtime", packages.filter((p) => p.shipped)),
    ...section("Build, test and development tools only", all.filter((p) => !p.shipped)),
  ].join("\n"),
);

const count = (list) => {
  const c = new Map();
  for (const p of list) c.set(p.license ?? "none", (c.get(p.license ?? "none") ?? 0) + 1);
  return [...c].sort((a, b) => b[1] - a[1]).map(([l, n]) => `  ${String(n).padStart(4)}  ${l}`).join("\n");
};
const lines = [
  `Rust (${target}): ${crates.length} crates, ${crates.filter((p) => p.shipped).length} linked into the pulse binary`,
  count(crates),
  `npm (${process.platform}/${process.arch}): ${packages.length} packages, ${packages.filter((p) => p.shipped).length} shipped`,
  count(packages),
  `Reviewed exceptions (build tools only): ${report.reviewed.length}`,
  ...report.reviewed.map((r) => `  ${r.ecosystem} ${r.package}: ${r.license}: ${r.why}`),
  `Without a license or notice file in the package: ${report.missingNotices.length}`,
  `Problems: ${report.problems.length}`,
  ...report.problems.map((r) => `  ${r.ecosystem} ${r.package}: ${r.why}`),
  `Reports: ${output}`,
];
console.log(lines.join("\n"));
process.exit(report.problems.length > 0 ? 1 : 0);
