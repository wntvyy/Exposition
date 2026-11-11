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
  /** Return false (or a promise of false) when the command is unavailable. */
  enabled?: () => boolean;
  run: () => void | Promise<void>;
}

type Listener = () => void;

class Registry {
  private views = new Map<string, ViewContribution>();
  private editors = new Map<string, EditorContribution>();
  private commands = new Map<string, CommandContribution>();
  private listeners = new Set<Listener>();
  private version = 0;

  registerView(view: ViewContribution): () => void {
    return this.add(this.views, view);
  }

  registerEditor(editor: EditorContribution): () => void {
    return this.add(this.editors, editor);
  }

  registerCommand(command: CommandContribution): () => void {
    return this.add(this.commands, command);
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
