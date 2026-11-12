// Monaco environment: bundle the editor worker and icon font locally (no CDN,
// CSP-compatible).
// Import Monaco only through this module so the environment is set first.
// `monaco-editor/editor` is the core editor API without the bundled language
// catalogue or editor contributions; Exposition registers the languages and
// the contributions its features rely on itself (below).
//
// Themes: the Exposition light/dark themes are defined here and Monaco follows
// the app theme automatically. Editors spread `editorDefaults` into their
// options and never call `monaco.editor.setTheme` directly.
import * as monaco from "monaco-editor/editor";
import EditorWorker from "monaco-editor/editor/editor.worker.js?worker";
// The codicon icon font (diff gutter +/-, find widget, folding, hovers). The
// core editor entry does not include it; this registers its @font-face, and
// Vite bundles codicon.ttf as a local asset (CSP font-src 'self').
import "monaco-editor/features/codicon/register";
// Editor contributions (each registers itself on import). The bare API has
// none: without them hover providers, marker hovers, completion providers and
// read-only messages are silently dead.
//   hover            provider hovers (Lean), diagnostic and decoration hovers
//   suggestController completion widget (Lean `\alpha` abbreviations)
//   gotoSymbol       F12 / Ctrl+Click / peek (Lean definition provider)
//   gotoError        F8 / Shift+F8 through diagnostics
//   readOnlyMessage  why a read-only editor refuses typing (SourceEditor)
//   find, folding, bracketMatching, comment, linesOperations, wordOperations,
//   multicursor, indentation, contextmenu + clipboard: the source editor's
//   ordinary editing commands.
// Not bundled: the link detector (`links`), which would open URLs found in
// source text; links in hovers go through `registerLinkOpener` below.
import "monaco-editor/features/hover/register";
import "monaco-editor/editor/contrib/suggest/browser/suggestController";
import "monaco-editor/features/snippet/register";
import "monaco-editor/features/gotoSymbol/register";
import "monaco-editor/features/gotoError/register";
import "monaco-editor/features/readOnlyMessage/register";
import "monaco-editor/features/find/register";
import "monaco-editor/features/folding/register";
import "monaco-editor/features/bracketMatching/register";
import "monaco-editor/features/comment/register";
import "monaco-editor/features/linesOperations/register";
import "monaco-editor/features/wordOperations/register";
import "monaco-editor/features/multicursor/register";
import "monaco-editor/features/indentation/register";
import "monaco-editor/features/contextmenu/register";
import "monaco-editor/features/clipboard/register";

import { applyMonacoTheme, defineMonacoThemes, editorDefaults, followAppTheme, MONACO_THEMES } from "../ui/monaco-theme";

self.MonacoEnvironment = {
  getWorker: () => new EditorWorker(),
};

defineMonacoThemes(monaco);
followAppTheme(monaco);

/** Re-apply the app theme (e.g. after a feature temporarily changed it). */
const syncTheme = () => applyMonacoTheme(monaco);

/**
 * Every link Monaco would open (a link in a hover's Markdown, e.g. a Lean
 * docstring) goes to this handler and is reported handled, so Monaco never
 * falls back to its own opener (`window.open`, which would navigate the
 * webview or open another window). Features install a handler that offers the
 * link safely (see `features/editor/links.ts`); until then links do nothing.
 */
let linkHandler: (uri: string) => void = () => {};

export function setEditorLinkHandler(handler: (uri: string) => void): void {
  linkHandler = handler;
}

monaco.editor.registerLinkOpener({
  open(resource) {
    linkHandler(resource.toString(true));
    return true;
  },
});

export { editorDefaults, monaco, MONACO_THEMES, syncTheme };
