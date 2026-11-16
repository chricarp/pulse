// The page the window shows while there is no daemon to load the UI from. Static: no script, a
// CSP that allows nothing but inline style, text escaped.

import { escapeHtml, withoutInvisible } from "./text.ts";

// Error text can come from the daemon or the system; it must not reorder or hide what is around it.
const text = (s: string) => escapeHtml(withoutInvisible(s));

export function statusPage(title: string, lines: readonly string[]): string {
  const body = lines.map((l) => `<p>${text(l)}</p>`).join("");
  const html = `<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'">
<title>Lictory Pulse</title><style>
:root{color-scheme:light dark;--fg:#1d1b20;--muted:#5f5b66;--bg:#fbfafc;--accent:#9333ea}
@media (prefers-color-scheme:dark){:root{--fg:#ece9f1;--muted:#a7a1b0;--bg:#141217}}
body{margin:0;min-height:100vh;display:grid;place-items:center;background:var(--bg);color:var(--fg);
font:15px/1.5 system-ui,-apple-system,"Segoe UI",sans-serif}
main{max-width:34rem;padding:2rem}h1{font-size:1.3rem;margin:0 0 .75rem}
h1::before{content:"";display:inline-block;width:.7rem;height:.7rem;border-radius:50%;
background:var(--accent);margin-right:.6rem;vertical-align:.08rem}p{color:var(--muted);margin:.4rem 0}
</style></head><body><main><h1>${text(title)}</h1>${body}</main></body></html>`;
  return `data:text/html;charset=utf-8,${encodeURIComponent(html)}`;
}
