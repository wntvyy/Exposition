// Top-level page navigation (Editor, Research, Agents, Graph).
//
// One page is shown at a time. A page is mounted on its first visit and stays
// mounted (hidden and inert) afterwards, so its state, sessions, and editors
// survive navigation. Pages are registered through `registry.registerPage`;
// the Editor page is the shell's own workbench layout.
import { create } from "zustand";

import { registry } from "./registry";

export const EDITOR_PAGE = "editor";

interface PagesState {
  active: string;
  /** Pages mounted so far (the active page is always included). */
  visited: string[];
}

export const usePages = create<PagesState>(() => ({ active: EDITOR_PAGE, visited: [EDITOR_PAGE] }));

export const pages = {
  /** Show a page by id. Unknown ids are ignored (returns false). */
  show(id: string): boolean {
    if (id !== EDITOR_PAGE && !registry.page(id)) return false;
    usePages.setState((s) => ({
      active: id,
      visited: s.visited.includes(id) ? s.visited : [...s.visited, id],
    }));
    return true;
  },
  active(): string {
    return usePages.getState().active;
  },
};

export function resetPagesForTests(): void {
  usePages.setState({ active: EDITOR_PAGE, visited: [EDITOR_PAGE] });
}
