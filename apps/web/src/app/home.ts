// The user's home directory as the daemon reports it (`SystemStatus.user_home`), so paths can
// be shown as `~/…` the way the user wrote them in config. Absolute paths stay in titles and
// copy buttons.

import { tildify, tildifyText } from "../lib/format";
import { createStore, useStore } from "../store/store";

export const userHome = createStore<string | null>(null);

export function useTildify(): (path: string) => string {
  const home = useStore(userHome, (h) => h);
  return (path) => tildify(path, home);
}

/** For prose from the daemon that mentions paths (a source's error). */
export function useTildifyText(): (text: string) => string {
  const home = useStore(userHome, (h) => h);
  return (text) => tildifyText(text, home);
}
