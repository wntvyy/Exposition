// The Graph page's toolbar row (the PageHeader's second row, region
// `graph-toolbar`) belongs to the page so the header stays mounted when the
// section changes; each section (Project, Mathematics) fills the row with its
// own tools through a portal.
import { createContext, useContext, type ReactNode } from "react";
import { createPortal } from "react-dom";

/**
 * The toolbar row's slot element. `undefined`: no page header around the
 * section (it renders its tools in place); `null`: the slot is not mounted yet.
 */
export const ToolbarSlot = createContext<HTMLElement | null | undefined>(undefined);

export function SectionToolbar({ label, children }: { label: string; children: ReactNode }) {
  const slot = useContext(ToolbarSlot);
  const bar = (
    <div className="xp-graph-toolbar" role="toolbar" aria-label={label}>
      {children}
    </div>
  );
  if (slot === undefined) return bar;
  return slot ? createPortal(bar, slot) : null;
}
