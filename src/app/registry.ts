// Workbench composition contract.
//
// Feature modules register their views and commands here; the workbench shell
// (owned by the UI owner) decides where and how they render. Features never
// reach into the shell's layout, and the shell never imports feature internals.
//
// Register from a feature's `register.ts`, imported once by `src/features/index.ts`.
import type { ComponentType, ReactNode } from "react";

/** Where a contributed view lives in the workbench. */
export type ViewLocation =
  /** Left sidebar: files, outline, references. */
  | "sidebar"
  /** Bottom panel: research, output, jobs, problems, terminal. */
  | "panel"
  /** Right contextual panel: agent tasks, review. */
  | "auxiliary";

export interface ViewContribution {
  id: string;
  location: ViewLocation;
  title: string;
  /** Icon component (lucide-react) shown in tab strips and activity bars. */
  icon: ComponentType<{ size?: number; "aria-hidden"?: boolean }>;
  /** Lower numbers sort first. */
  order?: number;
  render: () => ReactNode;
}

/** Something that can be opened in an editor tab. */
export interface EditorResource {
  /** Stable key used to deduplicate tabs, e.g. `file:main.typ`. */
  key: string;
  /** Workspace-relative path, when the resource is a project file. */
  path?: string;
  title: string;
}

export type DocumentMode = "source" | "page" | "split";

export interface EditorProps {
  resource: EditorResource;
  /** Requested document mode; editors that do not support a mode ignore it. */
  mode: DocumentMode;
  /** True when this editor group has keyboard focus. */
  active: boolean;
}

export interface EditorContribution {
  id: string;
  /** Higher priority wins when several editors match. */
  priority?: number;
  matches: (resource: EditorResource) => boolean;
  /** Modes this editor supports; `source` alone hides the mode switcher. */
  modes: DocumentMode[];
  component: ComponentType<EditorProps>;
}

export interface CommandContribution {
  id: string;
  title: string;
  /** Category shown in the command palette, e.g. "File", "Build". */
  category?: string;
  /** Keybinding in `Mod+Shift+P` form; `Mod` is Ctrl on Windows/Linux, Cmd on macOS. */
  keybinding?: string;
  /**
   * Where the keybinding fires. `"global"` also fires while focus is in a text
   * input or Monaco (unless that editor handled the key first); `"workbench"`
   * fires only outside text-editing surfaces. Defaults to `"global"` for chords
   * with Ctrl/Cmd/Alt or a function key (except text-editing chords such as
   * Mod+C or Mod+Z), and `"workbench"` otherwise. See `src/app/keybindings.ts`.
   */
  keybindingScope?: "global" | "workbench";
  /** Return false (or a promise of false) when the command is unavailable. */
  enabled?: () => boolean;
  run: () => void | Promise<void>;
}

/**
 * A top-level page (Editor, Research, Agents, Graph). The shell shows one page
 * at a time beside the activity rail; a page stays mounted (hidden and inert)
 * after its first visit, so switching pages never recreates its sessions,
 * tasks, or document models. The Editor page is built into the shell.
 */
export interface PageContribution {
  /** Stable id, e.g. `research`. `editor` is reserved for the shell. */
  id: string;
  title: string;
  icon: ComponentType<{ size?: number; "aria-hidden"?: boolean }>;
  /** Lower numbers sort first; the Editor page is 0. */
  order?: number;
  render: () => ReactNode;
}

/** An item in the status bar. */
export interface StatusItemContribution {
  id: string;
  alignment: "left" | "right";
  /** Lower numbers sort first within an alignment. */
  order?: number;
  render: () => ReactNode;
}

type Listener = () => void;

class Registry {
  private views = new Map<string, ViewContribution>();
  private editors = new Map<string, EditorContribution>();
  private commands = new Map<string, CommandContribution>();
  private statusItems = new Map<string, StatusItemContribution>();
  private pageMap = new Map<string, PageContribution>();
  private listeners = new Set<Listener>();
  private version = 0;

  registerPage(page: PageContribution): () => void {
    if (page.id === "editor") throw new Error("the editor page is built into the shell");
    return this.add(this.pageMap, page);
  }

  /** Registered pages (excluding the shell's Editor page), in order. */
  pages(): PageContribution[] {
    return [...this.pageMap.values()].sort(
      (a, b) => (a.order ?? 100) - (b.order ?? 100) || a.title.localeCompare(b.title),
    );
  }

  page(id: string): PageContribution | undefined {
    return this.pageMap.get(id);
  }

  registerView(view: ViewContribution): () => void {
    return this.add(this.views, view);
  }

  registerEditor(editor: EditorContribution): () => void {
    return this.add(this.editors, editor);
  }

  registerCommand(command: CommandContribution): () => void {
    return this.add(this.commands, command);
  }

  registerStatusItem(item: StatusItemContribution): () => void {
    return this.add(this.statusItems, item);
  }

  statusItemsAt(alignment: "left" | "right"): StatusItemContribution[] {
    return [...this.statusItems.values()]
      .filter((item) => item.alignment === alignment)
      .sort((a, b) => (a.order ?? 100) - (b.order ?? 100));
  }

  viewsAt(location: ViewLocation): ViewContribution[] {
    return [...this.views.values()]
      .filter((v) => v.location === location)
      .sort((a, b) => (a.order ?? 100) - (b.order ?? 100) || a.title.localeCompare(b.title));
  }

  editorFor(resource: EditorResource): EditorContribution | undefined {
    return [...this.editors.values()]
      .filter((e) => e.matches(resource))
      .sort((a, b) => (b.priority ?? 0) - (a.priority ?? 0))[0];
  }

  allCommands(): CommandContribution[] {
    return [...this.commands.values()].sort((a, b) =>
      `${a.category ?? ""}${a.title}`.localeCompare(`${b.category ?? ""}${b.title}`),
    );
  }

  command(id: string): CommandContribution | undefined {
    return this.commands.get(id);
  }

  async runCommand(id: string): Promise<void> {
    const command = this.commands.get(id);
    if (!command) throw new Error(`unknown command ${id}`);
    if (command.enabled && !command.enabled()) return;
    await command.run();
  }

  /** Subscribe to registration changes (for `useSyncExternalStore`). */
  subscribe = (listener: Listener): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  getVersion = (): number => this.version;

  private add<T extends { id: string }>(map: Map<string, T>, item: T): () => void {
    if (map.has(item.id)) throw new Error(`duplicate contribution id ${item.id}`);
    map.set(item.id, item);
    this.changed();
    return () => {
      if (map.get(item.id) === item) {
        map.delete(item.id);
        this.changed();
      }
    };
  }

  private changed() {
    this.version += 1;
    for (const listener of this.listeners) listener();
  }
}

export const registry = new Registry();
