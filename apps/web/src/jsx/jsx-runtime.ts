// The JSX runtime the app compiles to: React's, with text made visible (see `clean.ts`).
import { Fragment, jsx as reactJsx, jsxs as reactJsxs } from "react/jsx-runtime";

import { cleanProps } from "./clean";

export { Fragment };
export type { JSX } from "react/jsx-runtime";

export const jsx: typeof reactJsx = (type, props, key) => reactJsx(type, cleanProps(type, props), key);
export const jsxs: typeof reactJsxs = (type, props, key) => reactJsxs(type, cleanProps(type, props), key);
