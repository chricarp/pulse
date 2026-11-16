// Renders the menu model as the tray menu and as the window's menu bar. Rebuilt only when the
// model changes, which is only when a push from the daemon changed something.

import { Menu, type MenuItemConstructorOptions, Tray, nativeImage } from "electron";
import { join } from "node:path";

import type { Action, MenuItem, TrayModel } from "./menu.ts";

export function toTemplate(items: readonly MenuItem[], onAction: (a: Action) => void): MenuItemConstructorOptions[] {
  return items.map((item): MenuItemConstructorOptions => {
    switch (item.kind) {
      case "label":
        return { label: item.label, enabled: false };
      case "separator":
        return { type: "separator" };
      case "action":
        return { label: item.label, enabled: item.enabled, click: () => onAction(item.action) };
      case "check":
        return { label: item.label, type: "checkbox", checked: item.checked, click: () => onAction(item.action) };
      case "submenu":
        return { label: item.label, submenu: toTemplate(item.items, onAction) };
    }
  });
}

/** The window's menu bar (the app menu on macOS): Pulse's items plus the standard roles that
 * make copy, paste and window shortcuts work. */
export function applicationMenu(model: TrayModel, onAction: (a: Action) => void, devTools: boolean): Menu {
  const mac = process.platform === "darwin";
  const pulse: MenuItemConstructorOptions[] = toTemplate(model.items, onAction);
  const template: MenuItemConstructorOptions[] = [
    { label: "Pulse", submenu: mac ? [{ role: "about" }, { type: "separator" }, ...pulse, { type: "separator" }, { role: "hide" }, { role: "hideOthers" }] : pulse },
    { role: "editMenu" },
    {
      label: "View",
      submenu: [
        { role: "reload" },
        ...(devTools ? [{ role: "toggleDevTools" } as const] : []),
        { type: "separator" },
        { role: "resetZoom" },
        { role: "zoomIn" },
        { role: "zoomOut" },
        { type: "separator" },
        { role: "togglefullscreen" },
      ],
    },
    { role: "windowMenu" },
  ];
  return Menu.buildFromTemplate(template);
}

export class TrayView {
  private readonly tray: Tray;
  private last = "";
  private dim: boolean | null = null;

  constructor(
    private readonly iconDir: string,
    private readonly onAction: (a: Action) => void,
  ) {
    this.tray = new Tray(this.icon(false));
  }

  private icon(dim: boolean) {
    // macOS: a template image, which the system tints for light and dark menu bars.
    const name =
      process.platform === "darwin" ? "trayTemplate.png" : dim ? "tray-dim.png" : "tray.png";
    return nativeImage.createFromPath(join(this.iconDir, name));
  }

  render(model: TrayModel): void {
    const key = JSON.stringify(model);
    if (key === this.last) return;
    this.last = key;
    this.tray.setContextMenu(Menu.buildFromTemplate(toTemplate(model.items, this.onAction)));
    this.tray.setToolTip(model.tooltip);
    if (process.platform === "darwin") this.tray.setTitle(model.badge);
    if (model.dim !== this.dim && process.platform !== "darwin") this.tray.setImage(this.icon(model.dim));
    this.dim = model.dim;
  }

  destroy(): void {
    this.tray.destroy();
  }
}
