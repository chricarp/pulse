// Renders every icon the app needs from the one source, assets/icon.svg:
//   icon.png             1024 px, the app icon (electron-builder makes .icns and the Linux set from it)
//   tray.png, tray-dim.png        32 px tray icons for Linux (dim: paused, or no daemon)
//   trayTemplate.png, @2x         macOS menu bar template images: the badge with its letter cut out,
//                                 black on transparent, which macOS tints for the menu bar
// The SVG uses a system font for its letter, so the result depends slightly on the fonts installed
// where it is built.

import { Resvg } from "@resvg/resvg-js";
import { chmod, mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";

// resvg does not know `system-ui`; the SVG's generic `sans-serif` gets a font present on build
// machines of each platform (it falls back to any font if that one is missing).
const SANS = process.platform === "darwin" ? "Helvetica" : "Liberation Sans";

function render(svg, size) {
  return new Resvg(svg, {
    fitTo: { mode: "width", value: size },
    font: { loadSystemFonts: true, sansSerifFamily: SANS, defaultFontFamily: SANS },
  })
    .render()
    .asPng();
}

/** The badge as a mask: the rounded square, with the letter (and anything else drawn on it) cut out. */
function templateSvg(source) {
  const shape = /<rect\b[^>]*\/>/.exec(source)?.[0];
  if (!shape) throw new Error("assets/icon.svg: expected a <rect> as the badge");
  const viewBox = /viewBox="([^"]+)"/.exec(source)?.[1] ?? "0 0 64 64";
  const rest = source
    .replace(/^[\s\S]*?<svg\b[^>]*>/, "")
    .replace(/<\/svg>\s*$/, "")
    .replace(shape, "")
    .replace(/fill="[^"]*"/g, 'fill="#000"');
  const white = shape.replace(/fill="[^"]*"/, 'fill="#fff"');
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="${viewBox}"><defs><mask id="m">${white}${rest}</mask></defs>${shape.replace(/fill="[^"]*"/, 'fill="#000"').replace("/>", ' mask="url(#m)"/>')}</svg>`;
}

function dimSvg(source) {
  return source.replace(/(<svg\b[^>]*>)/, '$1<g opacity="0.45">').replace(/<\/svg>\s*$/, "</g></svg>");
}

export async function renderIcons(sourcePath, outDir) {
  const source = await readFile(sourcePath, "utf8");
  await mkdir(outDir, { recursive: true });
  const template = templateSvg(source);
  const outputs = [
    ["icon.png", render(source, 1024)],
    ["tray.png", render(source, 32)],
    ["tray-dim.png", render(dimSvg(source), 32)],
    ["trayTemplate.png", render(template, 16)],
    ["trayTemplate@2x.png", render(template, 32)],
  ];
  for (const [name, png] of outputs) {
    await writeFile(join(outDir, name), png);
    // Packaging copies these into the app as they are; not writable by others whatever the umask.
    await chmod(join(outDir, name), 0o644);
  }
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const [source = "../../assets/icon.svg", out = "build/icons"] = process.argv.slice(2);
  await renderIcons(source, out);
}
