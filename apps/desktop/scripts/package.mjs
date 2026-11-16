// Packages the app for this machine's platform and architecture with electron-builder:
// Linux → AppImage, macOS → dmg. Expects ../../target/release/pulse (scripts/build.sh builds it).
// The app's version is the binary's, so the app and the daemon it ships always agree.

import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const binary = join(root, "..", "..", "target", "release", "pulse");
if (!existsSync(binary)) {
  console.error(`missing ${binary}: build it first (cargo build --release -p pulse-cli, or scripts/build.sh)`);
  process.exit(1);
}
const version = /(\d+\.\d+\.\d+\S*)/.exec(execFileSync(binary, ["--version"], { encoding: "utf8" }))?.[1];
if (!version) {
  console.error("could not read the version of the pulse binary");
  process.exit(1);
}

const platform = { linux: "--linux", darwin: "--mac" }[process.platform];
const arch = { x64: "--x64", arm64: "--arm64" }[process.arch];
if (!platform || !arch) {
  console.error(`packaging is set up for Linux and macOS on x64 and arm64, not ${process.platform}/${process.arch}`);
  process.exit(1);
}

execFileSync(process.execPath, [join(root, "scripts", "build.mjs")], { stdio: "inherit", cwd: root });
const builder = join(root, "node_modules", ".bin", "electron-builder");
execFileSync(builder, [platform, arch, "--publish", "never", `--config.extraMetadata.version=${version}`], {
  stdio: "inherit",
  cwd: root,
});

console.log(`packaged Lictory Pulse ${version} into ${join(root, "release")}`);
