// Entry points of the equation editor. The same operations are available from
// the source editor (palette, keybinding, context menu) and from the page view
// (palette, keybinding, toolbar): each resolves the caret of the surface the
// user worked in last and opens the one shared draft in the Equation panel.
import { editors } from "../../../app/editors";
import { EDITOR_PAGE, pages } from "../../../app/pages";
import { registry } from "../../../app/registry";
import { monaco } from "../../../lib/monaco";
import { languageForPath } from "../core";
import type { Surface } from "./document";
import type { OpenMode } from "./draft";
import { equationStore } from "./store";
import { focusedSurface, showEquationPanel } from "./surfaces";

export const EQUATION_COMMANDS = {
  insertOrEdit: "equation.insertOrEdit",
  insertInline: "equation.insertInline",
  insertDisplay: "equation.insertDisplay",
  edit: "equation.edit",
  show: "equation.show",
} as const;

/** Insert an inline equation, or edit the one at the caret (both surfaces). */
export const EQUATION_KEYBINDING = "Mod+Shift+M";

/** The target of the keydown being dispatched, if a key runs the command now. */
function keyTarget(): Element | null | undefined {
  const event = (globalThis as { event?: Event }).event;
  if (typeof KeyboardEvent === "undefined" || !(event instanceof KeyboardEvent) || event.type !== "keydown") return undefined;
  return event.target instanceof Element ? event.target : null;
}

/** The active editor tab is a document with equation support (and the key, if any, was pressed in the editor area). */
export function equationCommandAvailable(): boolean {
  if (pages.active() !== EDITOR_PAGE) return false;
  const path = editors.active()?.resource.path;
  if (!path || !languageForPath(path)) return false;
  const target = keyTarget();
  if (target === undefined || target === null) return true;
  return target.closest('[data-region="editor"]') !== null;
}

function open(mode: OpenMode, preferred?: Surface): Promise<boolean> {
  return equationStore.open(mode, preferred ?? focusedSurface());
}

let registered = false;

export function registerEquationCommands(): void {
  if (registered) return;
  registered = true;
  registry.registerCommand({
    id: EQUATION_COMMANDS.insertOrEdit,
    title: "Insert or Edit Equation",
    category: "Equation",
    keybinding: EQUATION_KEYBINDING,
    keybindingScope: "global",
    enabled: equationCommandAvailable,
    run: async () => {
      await open("auto");
    },
  });
  registry.registerCommand({
    id: EQUATION_COMMANDS.insertInline,
    title: "Insert Inline Equation",
    category: "Equation",
    enabled: equationCommandAvailable,
    run: async () => {
      await open("inline");
    },
  });
  registry.registerCommand({
    id: EQUATION_COMMANDS.insertDisplay,
    title: "Insert Display Equation",
    category: "Equation",
    enabled: equationCommandAvailable,
    run: async () => {
      await open("display");
    },
  });
  registry.registerCommand({
    id: EQUATION_COMMANDS.edit,
    title: "Edit Equation",
    category: "Equation",
    enabled: equationCommandAvailable,
    run: async () => {
      await open("edit");
    },
  });
  registry.registerCommand({
    id: EQUATION_COMMANDS.show,
    title: "Show Equation Panel",
    category: "View",
    run: showEquationPanel,
  });

  // The source editor's context menu (math documents only).
  const precondition = "editorLangId == latex || editorLangId == typst || editorLangId == markdown";
  monaco.editor.addEditorAction({
    id: "exposition.equation.insert",
    label: "Insert Equation",
    contextMenuGroupId: "1_modification",
    contextMenuOrder: 10,
    precondition,
    run: () => void open("inline", "source"),
  });
  monaco.editor.addEditorAction({
    id: "exposition.equation.insertDisplay",
    label: "Insert Display Equation",
    contextMenuGroupId: "1_modification",
    contextMenuOrder: 11,
    precondition,
    run: () => void open("display", "source"),
  });
  monaco.editor.addEditorAction({
    id: "exposition.equation.edit",
    label: "Edit Equation",
    contextMenuGroupId: "1_modification",
    contextMenuOrder: 12,
    precondition,
    run: () => void open("edit", "source"),
  });
}
