// Navigation into the Graph page from elsewhere (the Editor's dependency
// inspector, commands): which subsection to show and which entity to select.
import { create } from "zustand";

import { pages } from "../../app/pages";

export type GraphSection = "project" | "mathematics";

export const GRAPH_PAGE_ID = "graph";

/** Which subsection the Graph page shows. */
export const useGraphPage = create<{ section: GraphSection }>(() => ({ section: "project" }));

export interface SelectRequest {
  section: GraphSection;
  entity: string;
  nonce: number;
}

/** A pending "select this entity" request for a subsection. */
export const useGraphNavigation = create<{ request: SelectRequest | null }>(() => ({ request: null }));

export function showGraphSection(section: GraphSection, options: { focus?: boolean } = {}): void {
  pages.show(GRAPH_PAGE_ID, { focus: options.focus });
  useGraphPage.setState({ section });
}

/** Show the Graph page (subsection `section`) with `entity` selected and in view. */
export function openInGraph(entity: string, section: GraphSection = "project"): void {
  showGraphSection(section, { focus: true });
  useGraphNavigation.setState((s) => ({ request: { section, entity, nonce: (s.request?.nonce ?? 0) + 1 } }));
}

/** Take the pending request for `section`, if any (consumed once). */
export function takeRequest(section: GraphSection, lastNonce: number): SelectRequest | null {
  const request = useGraphNavigation.getState().request;
  return request && request.section === section && request.nonce > lastNonce ? request : null;
}
