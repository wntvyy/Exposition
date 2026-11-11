// Monaco environment: bundle the editor worker locally (no CDN, CSP-compatible).
// Import Monaco only through this module so the environment is set first.
// `monaco-editor/editor` is the core editor API without the bundled language
// catalogue; Exposition registers the languages it needs itself.
import * as monaco from "monaco-editor/editor";
import EditorWorker from "monaco-editor/editor/editor.worker.js?worker";

self.MonacoEnvironment = {
  getWorker: () => new EditorWorker(),
};

export { monaco };
