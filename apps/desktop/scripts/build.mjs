// Bundles the main process and the preload into dist/, and renders the icons into build/icons.
// The bundles have no runtime dependencies besides Electron itself, so the packaged app carries
// no node_modules.
//
//   node scripts/build.mjs          the app
//   node scripts/build.mjs --e2e    also the end-to-end harness (dist-e2e/, never packaged)

import { build } from "esbuild";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

import { renderIcons } from "./icons.mjs";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const common = {
  bundle: true,
  platform: "node",
  target: "node22",
  format: "cjs",
  external: ["electron"],
  sourcemap: "linked",
  logLevel: "warning",
  legalComments: "none",
};

await renderIcons(join(root, "..", "..", "assets", "icon.svg"), join(root, "build", "icons"));
await build({ ...common, entryPoints: [join(root, "src", "index.ts")], outfile: join(root, "dist", "main.cjs") });
await build({ ...common, entryPoints: [join(root, "src", "preload.ts")], outfile: join(root, "dist", "preload.cjs"), sourcemap: false });
if (process.argv.includes("--e2e")) {
  await build({ ...common, entryPoints: [join(root, "e2e", "harness.ts")], outfile: join(root, "dist-e2e", "harness.cjs") });
  await build({ ...common, entryPoints: [join(root, "src", "preload.ts")], outfile: join(root, "dist-e2e", "preload.cjs"), sourcemap: false });
}
