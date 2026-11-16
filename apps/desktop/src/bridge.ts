// What the preload exposes to the page as `window.pulseDesktop`, and nothing more. The web UI may
// check for it to offer desktop-only actions; it must work without it (in a browser).

export interface PulseDesktopBridge {
  /** Bumped when the bridge changes incompatibly. */
  readonly version: 1;
  readonly platform: string;
  /**
   * Shows a file or directory in the system file manager (selected, not opened). Resolves to
   * `false` if the path is not absolute or does not exist.
   */
  revealPath(path: string): Promise<boolean>;
}

export const REVEAL_PATH_CHANNEL = "pulse:reveal-path";
