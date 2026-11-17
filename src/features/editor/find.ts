// Find in the active editor: Monaco's own Find widget (match highlighting,
// count, next/previous, case/word/regex options) over the active text buffer,
// unsaved edits included. Nothing here searches text itself, and finding
// never edits, saves, or touches the project graph.
//
// Scope:
// - Source and Split views: "Edit: Find" opens the Find widget of the source
//   editor (in Split, the source side).
// - Page view (no page search exists): "Edit: Find in Source" switches the
//   document to Split, focuses its source editor, and opens Find there. It is
//   named for what it searches; rendered page text is not searched.
//
// Ctrl+F / ⌘F is focus-aware: inside a source editor Monaco handles it
// itself; elsewhere in the editor area (the page view, the editor tabs) these
// commands take it; in other regions (Files, terminals, panels, their text
// fields) and on other pages the key is left to them.
import { editors, useEditors } from "../../app/editors";
import { EDITOR_PAGE, pages } from "../../app/pages";
import { registry, type DocumentMode } from "../../app/registry";
import { monaco } from "../../lib/monaco";
import { requestSourceFocus } from "./sourceFocus";

export const FIND_COMMAND_ID = "editor.find";
export const FIND_IN_SOURCE_COMMAND_ID = "editor.findInSource";
export const FIND_KEYBINDING = "Mod+F";
/** Monaco's Find action (the find contribution is bundled in `lib/monaco`). */
export const MONACO_FIND_ACTION = "actions.find";

export type FindRoute = "source" | "pageToSource";

/**
 * Which Find applies to the active editor: its source editor ("source"), the
 * source of a document shown as pages ("pageToSource"), or none.
 */
export function findRoute(mode: DocumentMode | undefined, modes: readonly DocumentMode[], hasSourceEditor: boolean): FindRoute | null {
  if (mode === "page") return modes.includes("split") && modes.includes("source") ? "pageToSource" : null;
  return hasSourceEditor ? "source" : null;
}

/**
 * Whether Ctrl+F pressed at `target` is Find's: the editor area of the shown
 * Editor page (or nothing focused there). Text fields, terminals, and panels
 * elsewhere, and other pages, keep the key.
 */
export function findAllowedFrom(target: Element | null): boolean {
  if (pages.active() !== EDITOR_PAGE) return false;
  if (!target || target === document.body || target === document.documentElement) return true;
  return target.closest('[data-region="editor"]') !== null;
}

/**
 * The target of the keydown being dispatched, if a key runs the command now;
 * undefined when it runs otherwise (palette, menu, button), which always may.
 */
function keyTarget(): Element | null | undefined {
  const event = (globalThis as { event?: Event }).event;
  if (typeof KeyboardEvent === "undefined" || !(event instanceof KeyboardEvent) || event.type !== "keydown") return undefined;
  return event.target instanceof Element ? event.target : null;
}
/** The shown tab panel of the active editor group. */
function activePanel(): HTMLElement | null {
  if (typeof document === "undefined") return null;
  const { activeGroup } = useEditors.getState();
  const group = document.querySelector<HTMLElement>(`.wb-editor-group[data-group="${activeGroup}"]`);
  if (!group) return null;
  return [...group.querySelectorAll<HTMLElement>('[role="tabpanel"]')].find((p) => !p.hasAttribute("inert")) ?? null;
}

/** The Monaco source editor shown by the active editor tab, if any. */
export function activeSourceEditor(): monaco.editor.ICodeEditor | null {
  const panel = activePanel();
  if (!panel) return null;
  return monaco.editor.getEditors().find((e) => panel.contains(e.getContainerDomNode())) ?? null;
}

function currentRoute(): FindRoute | null {
  const active = editors.active();
  if (!active) return null;
  const modes = registry.editorFor(active.resource)?.modes ?? [];
  const mode = modes.includes(active.mode) ? active.mode : modes[0];
  return findRoute(mode, modes, activeSourceEditor() !== null);
}

function allowedNow(): boolean {
  const target = keyTarget();
  return target === undefined || findAllowedFrom(target);
}

/** Open Monaco's Find widget in `editor` (it takes focus; Escape returns it). */
export async function openFindWidget(editor: monaco.editor.ICodeEditor): Promise<void> {
  editor.focus();
  await editor.getAction(MONACO_FIND_ACTION)?.run();
}

async function waitForSourceEditor(timeoutMs = 5000): Promise<monaco.editor.ICodeEditor | null> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const editor = activeSourceEditor();
    if (editor?.getModel()) return editor;
    await new Promise((r) => setTimeout(r, 50));
  }
  return null;
}

/** Find in the active source editor. */
export async function findInActiveEditor(): Promise<void> {
  pages.show(EDITOR_PAGE);
  const editor = activeSourceEditor();
  if (editor) await openFindWidget(editor);
}

/** Page view: show the source beside the pages, focus it, and open Find there. */
export async function findInSource(): Promise<void> {
  const active = editors.active();
  if (!active) return;
  pages.show(EDITOR_PAGE);
  if (active.mode === "page") {
    requestSourceFocus(active.resource.key);
    editors.setMode(active.resource.key, "split");
  }
  const editor = await waitForSourceEditor();
  if (editor) await openFindWidget(editor);
}

let registered = false;

/** Register the Find commands and the editor context-menu entry (once). */
export function registerFind(): void {
  if (registered) return;
  registered = true;
  registry.registerCommand({
    id: FIND_COMMAND_ID,
    title: "Find",
    category: "Edit",
    keybinding: FIND_KEYBINDING,
    enabled: () => allowedNow() && currentRoute() === "source",
    run: findInActiveEditor,
  });
  registry.registerCommand({
    id: FIND_IN_SOURCE_COMMAND_ID,
    title: "Find in Source",
    category: "Edit",
    keybinding: FIND_KEYBINDING,
    enabled: () => allowedNow() && currentRoute() === "pageToSource",
    run: findInSource,
  });
  // The source editor's context menu lists Find (Monaco's own action).
  monaco.editor.addEditorAction({
    id: "exposition.find",
    label: "Find",
    contextMenuGroupId: "navigation",
    contextMenuOrder: 0,
    run: (editor) => editor.getAction(MONACO_FIND_ACTION)?.run(),
  });
}
