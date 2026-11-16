// A handful of 16px line icons, drawn for this app. `currentColor` throughout.

import type { SVGProps } from "react";

const paths = {
  feed: "M2.5 4h11M2.5 8h11M2.5 12h7",
  runs: "M5 3.5v9l7-4.5z",
  rules: "M3 3.5h4M3 8h10M3 12.5h7M10 2v3M6 6.5v3M12 11v3",
  agents: "M8 7a2.5 2.5 0 1 0 0-5 2.5 2.5 0 0 0 0 5zM3 14c.6-2.6 2.6-4 5-4s4.4 1.4 5 4",
  sources: "M8 8m-1.5 0a1.5 1.5 0 1 0 3 0a1.5 1.5 0 1 0-3 0M4.5 4.5a5 5 0 0 0 0 7M11.5 4.5a5 5 0 0 1 0 7M2.5 2.5a8 8 0 0 0 0 11M13.5 2.5a8 8 0 0 1 0 11",
  projects: "M2 4.5h4l1.5 1.5H14v6.5H2z",
  system: "M8 10a2 2 0 1 0 0-4 2 2 0 0 0 0 4zM8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M3.4 12.6l1.4-1.4M11.2 4.8l1.4-1.4",
  pause: "M5.5 3.5v9M10.5 3.5v9",
  play: "M5 3.5v9l7-4.5z",
  check: "M3 8.5l3 3 7-7",
  x: "M4 4l8 8M12 4l-8 8",
  stop: "M4.5 4.5h7v7h-7z",
  rerun: "M12.5 6A5 5 0 1 0 13 9M12.5 2.5V6H9",
  copy: "M5.5 5.5h7v7h-7zM3.5 10.5v-7h7",
  search: "M7 12a5 5 0 1 0 0-10 5 5 0 0 0 0 10zM14 14l-3.5-3.5",
  loop: "M3 8a5 5 0 0 1 8.5-3.5L13 6M13 2.5V6H9.5M13 8a5 5 0 0 1-8.5 3.5L3 10M3 13.5V10h3.5",
  alert: "M8 2l6.5 11.5h-13zM8 6.5v3M8 11.5v.5",
  clock: "M8 14.5a6.5 6.5 0 1 0 0-13 6.5 6.5 0 0 0 0 13zM8 4.5V8l2.5 1.5",
  plus: "M8 3v10M3 8h10",
  chevron: "M6 4l4 4-4 4",
  back: "M10 4L6 8l4 4",
  link: "M7 9a3 3 0 0 0 4.2 0l2-2a3 3 0 0 0-4.2-4.2l-.6.6M9 7a3 3 0 0 0-4.2 0l-2 2a3 3 0 0 0 4.2 4.2l.6-.6",
  tool: "M10 2.5a3.5 3.5 0 0 0-3.3 4.6L2.5 11.3l2.2 2.2 4.2-4.2A3.5 3.5 0 0 0 13.5 6l-2 2-2-2 2-2a3.5 3.5 0 0 0-1.5-1.5z",
  key: "M10 9.5a3.5 3.5 0 1 0-3.4-2.6L2.5 11v2.5H5V12h1.5v-1.5H8l.6-.6A3.5 3.5 0 0 0 10 9.5z",
  sun: "M8 11a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM8 1v1.5M8 13.5V15M1 8h1.5M13.5 8H15M3 3l1 1M12 12l1 1M3 13l1-1M12 4l1-1",
  moon: "M13 9.5A5.5 5.5 0 1 1 6.5 3a4.5 4.5 0 0 0 6.5 6.5z",
  monitor: "M2 3h12v8H2zM6 14h4M8 11v3",
  shield: "M8 1.5l5.5 2v4c0 3.2-2.3 5.8-5.5 7-3.2-1.2-5.5-3.8-5.5-7v-4z",
  filter: "M2 3h12l-4.5 5.5V13l-3 1.5v-6z",
  trash: "M3 4.5h10M6.5 4.5V3h3v1.5M4.5 4.5l.5 9h6l.5-9",
  edit: "M10.5 2.5l3 3-8 8h-3v-3z",
  eye: "M1.5 8s2.5-4.5 6.5-4.5S14.5 8 14.5 8 12 12.5 8 12.5 1.5 8 1.5 8zM8 10a2 2 0 1 0 0-4 2 2 0 0 0 0 4z",
  arrowDown: "M8 3v10M4 9l4 4 4-4",
  dot: "M8 8m-3 0a3 3 0 1 0 6 0a3 3 0 1 0-6 0",
  flask: "M6 2h4M6.5 2v4L3 13h10L9.5 6V2",
} as const;

export type IconName = keyof typeof paths;

export function Icon({ name, size = 16, ...rest }: { name: IconName; size?: number } & SVGProps<SVGSVGElement>) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.4}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
      {...rest}
    >
      <path d={paths[name]} />
    </svg>
  );
}
