// Monaco environment: bundle the editor worker and icon font locally (no CDN,
// CSP-compatible).
// Import Monaco only through this module so the environment is set first.
// `monaco-editor/editor` is the core editor API without the bundled language
// catalogue; Exposition registers the languages it needs itself.
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

import { applyMonacoTheme, defineMonacoThemes, editorDefaults, followAppTheme, MONACO_THEMES } from "../ui/monaco-theme";

self.MonacoEnvironment = {
  getWorker: () => new EditorWorker(),
};

defineMonacoThemes(monaco);
followAppTheme(monaco);

/** Re-apply the app theme (e.g. after a feature temporarily changed it). */
const syncTheme = () => applyMonacoTheme(monaco);

export { editorDefaults, monaco, MONACO_THEMES, syncTheme };
