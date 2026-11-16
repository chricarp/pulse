// electron-builder afterPack hook. Electron's Linux archives can carry group- and world-writable
// files, and electron-builder copies modes as they are into the AppImage. Nothing in an installed
// app should be writable by other users. Symlinks are not followed.
const { chmod, lstat, readdir } = require("node:fs/promises");
const { join } = require("node:path");

async function tighten(path) {
  const st = await lstat(path);
  if (st.isSymbolicLink()) return 0;
  let changed = 0;
  if (st.mode & 0o022) {
    await chmod(path, st.mode & 0o7755);
    changed++;
  }
  if (st.isDirectory()) for (const e of await readdir(path)) changed += await tighten(join(path, e));
  return changed;
}

exports.default = async function afterPack(context) {
  const changed = await tighten(context.appOutDir);
  if (changed > 0) console.log(`  • removed group/world write permission from ${changed} files`);
};
