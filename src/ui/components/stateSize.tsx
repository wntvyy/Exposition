import { createContext, useContext, type ReactNode } from "react";

/**
 * How region states (empty, loading, error, unavailable) render where they
 * appear: `page` for pages and the editor area (centered icon, title, text,
 * actions), `compact` for panels, sidebars, auxiliary views, and inspector
 * sections (one muted line aligned with the content, inline actions).
 *
 * The shell provides `compact` for the sidebar, bottom panel, and auxiliary
 * regions; InspectorBody provides it for inspectors. A state's own `size`
 * prop wins.
 */
export type StateSize = "page" | "compact";

const StateSizeContext = createContext<StateSize>("page");

export function StateSizeProvider({ value, children }: { value: StateSize; children: ReactNode }) {
  return <StateSizeContext.Provider value={value}>{children}</StateSizeContext.Provider>;
}

export function useStateSize(explicit?: StateSize): StateSize {
  const inherited = useContext(StateSizeContext);
  return explicit ?? inherited;
}
