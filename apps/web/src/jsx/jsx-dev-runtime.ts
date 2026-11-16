// The development JSX runtime, with text made visible like the production one (`clean.ts`).
import { Fragment, jsxDEV as reactJsxDEV } from "react/jsx-dev-runtime";

import { cleanProps } from "./clean";

export { Fragment };
export type { JSX } from "react/jsx-dev-runtime";

export const jsxDEV: typeof reactJsxDEV = (type, props, key, isStatic, source, self) =>
  reactJsxDEV(type, cleanProps(type, props), key, isStatic, source, self);
