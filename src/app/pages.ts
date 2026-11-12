// Top-level page navigation (Editor, Research, Agents, Graph).
//
// One page is shown at a time. A page is mounted on its first visit and stays
// mounted (hidden and inert) afterwards, so its state, sessions, and editors
// survive navigation. Pages are registered through `registry.registerPage`;
// the Editor page is the shell's own workbench layout.
//
// Every page gets a palette command "View: Show <Title>" (registered by the
// shell, see `registerPageCommands`); the four product pages also have fixed
// keybindings (`PAGE_KEYBINDINGS`). Showing a page from a command moves
// keyboard focus into it; choosing it on the activity rail leaves focus there.
import { create } from "zustand";

import { registry, type CommandContribution } from "./registry";

export const EDITOR_PAGE = "editor";

/** Title of the shell's built-in Editor page. */
export const EDITOR_PAGE_TITLE = "Editor";

/**
 * Keybindings of the product pages, in rail order. `Mod+<digit>` is never a
 * text-editing chord (Monaco and the page editor bind none of them), so the
 * bindings also work while an editor has focus.
 */
export const PAGE_KEYBINDINGS: Readonly<Record<string, string>> = {
  editor: "Mod+1",
  research: "Mod+2",
  agents: "Mod+3",
  graph: "Mod+4",
};

/** Command id of the "Show <page>" command. */
export const showPageCommandId = (id: string) => `workbench.action.showPage.${id}`;

interface PagesState {
  active: string;
  /** Pages mounted so far (the active page is always included). */
  visited: string[];
  /** Bumped when a page should take keyboard focus once shown. */
  focusRequest: number;
}

export const usePages = create<PagesState>(() => ({ active: EDITOR_PAGE, visited: [EDITOR_PAGE], focusRequest: 0 }));

export interface ShowPageOptions {
  /** Move keyboard focus into the page once it is shown (commands, keybindings). */
  focus?: boolean;
}

export const pages = {
  /** Show a page by id. Unknown ids are ignored (returns false). */
  show(id: string, options: ShowPageOptions = {}): boolean {
    if (id !== EDITOR_PAGE && !registry.page(id)) return false;
    usePages.setState((s) => ({
      active: id,
      visited: s.visited.includes(id) ? s.visited : [...s.visited, id],
      focusRequest: options.focus ? s.focusRequest + 1 : s.focusRequest,
    }));
    return true;
  },
  active(): string {
    return usePages.getState().active;
  },
  /** Every page in rail order: the Editor page first, then registered pages. */
  all(): { id: string; title: string }[] {
    return [{ id: EDITOR_PAGE, title: EDITOR_PAGE_TITLE }, ...registry.pages().map(({ id, title }) => ({ id, title }))];
  },
};

function pageCommand(id: string, title: string): CommandContribution {
  return {
    id: showPageCommandId(id),
    title: `Show ${title}`,
    category: "View",
    keybinding: PAGE_KEYBINDINGS[id],
    run: () => void pages.show(id, { focus: true }),
  };
}

/**
 * Keep one "View: Show <Title>" command per page (the Editor page and every
 * registered page) as pages register and unregister. Returns a disposer.
 */
export function registerPageCommands(): () => void {
  const registered = new Map<string, { title: string; dispose: () => void }>();
  let syncing = false;
  const sync = () => {
    if (syncing) return;
    syncing = true;
    try {
      const current = pages.all();
      for (const [id, entry] of registered) {
        if (!current.some((p) => p.id === id && p.title === entry.title)) {
          entry.dispose();
          registered.delete(id);
        }
      }
      for (const page of current) {
        if (!registered.has(page.id)) {
          registered.set(page.id, { title: page.title, dispose: registry.registerCommand(pageCommand(page.id, page.title)) });
        }
      }
    } finally {
      syncing = false;
    }
  };
  sync();
  const unsubscribe = registry.subscribe(sync);
  return () => {
    unsubscribe();
    for (const entry of registered.values()) entry.dispose();
    registered.clear();
  };
}

export function resetPagesForTests(): void {
  usePages.setState({ active: EDITOR_PAGE, visited: [EDITOR_PAGE], focusRequest: 0 });
}
