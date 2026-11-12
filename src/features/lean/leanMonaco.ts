// Monaco support for Lean: syntax highlighting, Unicode abbreviation input,
// goal queries on cursor movement, diagnostics as markers, sorry highlighting,
// hover, and definitions. The source editor itself is the shared SourceEditor;
// this module only attaches to editors whose model is a Lean document.
//
// Server output is untrusted: hovers render as Markdown without HTML or command
// links (`isTrusted: false`, `supportHtml: false`); goals are plain text.
import type { DocumentId } from "../../bindings/DocumentId";
import type { LeanDocumentState } from "../../bindings/LeanDocumentState";
import type { TextRange } from "../../bindings/TextRange";
import { editors } from "../../app/editors";
import { notify } from "../../app/notifications";
import { commands, errorMessage } from "../../lib/ipc";
import { monaco } from "../../lib/monaco";
import { fileKey, handleForPath, openHandles, type DocumentHandle } from "../editor";
import { ensureLanguage } from "../editor/languages";
import { abbreviationsWithPrefix, expandAbbreviation, isTerminator } from "./abbreviations";
import { clearCursorEditor, onLeanDocument, setCursorEditor, syncedCursor, useLean } from "./store";
import "./lean.css";

const LANGUAGE = "lean";

const leanMonarch: monaco.languages.IMonarchLanguage = {
  defaultToken: "",
  keywords: [
    "theorem", "lemma", "def", "example", "abbrev", "instance", "structure", "class", "inductive", "where",
    "extends", "by", "fun", "match", "with", "let", "have", "show", "from", "calc", "do", "if", "then", "else",
    "namespace", "section", "end", "open", "import", "variable", "universe", "set_option", "attribute",
    "deriving", "private", "protected", "noncomputable", "partial", "unsafe", "mutual", "notation", "infix",
    "infixl", "infixr", "prefix", "postfix", "macro", "macro_rules", "syntax", "elab", "termination_by",
    "decreasing_by", "at", "in", "for", "return", "opaque", "axiom", "local", "scoped", "export", "nomatch",
    "suffices", "obtain", "rcases", "intro", "intros", "exact", "apply", "refine", "rfl", "simp", "rw",
    "induction", "cases", "constructor", "omega", "decide", "unfold", "exists", "use", "trivial",
  ],
  incomplete: ["sorry", "admit", "stop"],
  types: ["Type", "Prop", "Sort", "Nat", "Int", "Bool", "True", "False", "String", "List", "Option"],
  tokenizer: {
    root: [
      [/--.*$/, "comment"],
      [/\/--/, "comment.doc", "@comment"],
      [/\/-/, "comment", "@comment"],
      [/@\[[^\]]*\]/, "annotation"],
      [/#[a-zA-Z_]+/, "keyword.directive"],
      [/[a-zA-Z_][\w'!?.]*/, {
        cases: { "@incomplete": "invalid", "@keywords": "keyword", "@types": "type", "@default": "identifier" },
      }],
      [/\d+(\.\d+)?/, "number"],
      [/"([^"\\]|\\.)*"/, "string"],
      [/'([^'\\]|\\.)'/, "string"],
      [/[⊢∀∃→←↔¬∧∨≤≥≠∈∉⊆⊂∩∪×∘λ⟨⟩]/, "operator"],
      [/[{}()[\]]/, "delimiter.bracket"],
    ],
    comment: [
      [/\/-/, "comment", "@push"],
      [/-\//, "comment", "@pop"],
      [/[^/-]+/, "comment"],
      [/./, "comment"],
    ],
  },
};

const languageConfiguration: monaco.languages.LanguageConfiguration = {
  comments: { lineComment: "--", blockComment: ["/-", "-/"] },
  brackets: [["(", ")"], ["[", "]"], ["{", "}"], ["⟨", "⟩"]],
  autoClosingPairs: [
    { open: "(", close: ")" },
    { open: "[", close: "]" },
    { open: "{", close: "}" },
    { open: "⟨", close: "⟩" },
    { open: '"', close: '"', notIn: ["string", "comment"] },
  ],
  wordPattern: /[^\s()[\]{}⟨⟩,:;."`]+/,
};

function handleForModel(model: monaco.editor.ITextModel | null): DocumentHandle | undefined {
  return model ? openHandles().find((h) => h.model === model) : undefined;
}

const isLeanModel = (model: monaco.editor.ITextModel | null) => model?.getLanguageId() === LANGUAGE;

function toMonacoRange(model: monaco.editor.ITextModel, range: TextRange): monaco.Range {
  const a = model.getPositionAt(range.start);
  const b = model.getPositionAt(range.end);
  return new monaco.Range(a.lineNumber, a.column, b.lineNumber, b.column);
}

/** The revision both this view and Lean have, or null while they differ. */
function syncedRevision(handle: DocumentHandle): number | null {
  const state = useLean.getState().docs[handle.id];
  if (!state || handle.sync.pendingCount > 0 || handle.sync.revision !== state.revision) return null;
  return state.revision;
}

// ----- per-editor behavior -------------------------------------------------------

function attachEditor(editor: monaco.editor.ICodeEditor) {
  const disposables: monaco.IDisposable[] = [];
  const focusCursor = () => {
    const handle = handleForModel(editor.getModel());
    if (handle && isLeanModel(editor.getModel()) && editor.hasTextFocus()) setCursorEditor(editor, handle.id);
  };
  disposables.push(
    editor.onDidFocusEditorText(focusCursor),
    editor.onDidChangeCursorPosition(() => {
      if (editor.hasTextFocus()) focusCursor();
    }),
    editor.onDidChangeModelContent((e) => {
      const model = editor.getModel();
      if (!model || !isLeanModel(model) || !editor.hasTextFocus()) return;
      if (e.isUndoing || e.isRedoing || e.isFlush || e.changes.length !== 1) return;
      const change = e.changes[0];
      if (!change || change.rangeLength !== 0 || !isTerminator(change.text)) return;
      const line = change.range.startLineNumber;
      const before = model.getLineContent(line).slice(0, change.range.startColumn - 1);
      const expansion = expandAbbreviation(before);
      if (!expansion) return;
      // Replace `\name` after this change event completes; it is an ordinary
      // source edit (document transaction, shared undo).
      queueMicrotask(() => {
        if (editor.getModel() !== model) return;
        editor.executeEdits("lean.abbreviation", [
          {
            range: new monaco.Range(line, expansion.start + 1, line, change.range.startColumn),
            text: expansion.replacement,
          },
        ]);
      });
    }),
  );
  editor.onDidDispose(() => {
    clearCursorEditor(editor);
    for (const d of disposables) d.dispose();
  });
}

// ----- markers and sorry decorations ------------------------------------------------

const MARKER_OWNER = "lean";
const sorryDecorations = new WeakMap<monaco.editor.ITextModel, string[]>();

function updateModel(state: LeanDocumentState | null, document: DocumentId) {
  const handle = openHandles().find((h) => h.id === document);
  if (!handle || handle.closed) return;
  const model = handle.model;
  if (!state) {
    monaco.editor.setModelMarkers(model, MARKER_OWNER, []);
    sorryDecorations.set(model, model.deltaDecorations(sorryDecorations.get(model) ?? [], []));
    return;
  }
  // Positions are only meaningful for the revision the view holds; markers
  // already placed move with later edits until Lean reports again.
  if (state.diagnosticsRevision !== handle.sync.revision || handle.sync.pendingCount > 0) return;
  const markers: monaco.editor.IMarkerData[] = state.diagnostics.flatMap((d) => {
    if (!d.location) return [];
    const r = toMonacoRange(model, d.location.range);
    const severity =
      d.severity === "error"
        ? monaco.MarkerSeverity.Error
        : d.severity === "warning"
          ? monaco.MarkerSeverity.Warning
          : monaco.MarkerSeverity.Info;
    return [{ severity, message: d.message, source: "Lean", startLineNumber: r.startLineNumber, startColumn: r.startColumn, endLineNumber: r.endLineNumber, endColumn: r.endColumn }];
  });
  monaco.editor.setModelMarkers(model, MARKER_OWNER, markers);
  const status = state.status;
  const tokens =
    status.kind === "checkedWithSorry" && status.revision === handle.sync.revision
      ? status.locations.flatMap((l) => l.tokens)
      : [];
  sorryDecorations.set(
    model,
    model.deltaDecorations(
      sorryDecorations.get(model) ?? [],
      tokens.map((t) => ({
        range: toMonacoRange(model, t),
        options: {
          inlineClassName: "exp-lean-sorry",
          hoverMessage: { value: "Incomplete proof: `sorry` admits this goal without proof." },
        },
      })),
    ),
  );
}

// ----- navigation ----------------------------------------------------------------------

/** Open a workspace file in the editor area and select a position in it. */
export async function revealInEditor(path: string, target: { range?: TextRange | null; line?: number; character?: number }) {
  editors.open({ key: fileKey(path), path, title: path.split("/").pop() ?? path }, { preview: false });
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    const handle = handleForPath(path);
    const editor = handle && monaco.editor.getEditors().find((e) => e.getModel() === handle.model);
    if (handle && editor) {
      const model = handle.model;
      const range = target.range
        ? toMonacoRange(model, target.range)
        : new monaco.Range((target.line ?? 0) + 1, (target.character ?? 0) + 1, (target.line ?? 0) + 1, (target.character ?? 0) + 1);
      editor.setSelection(new monaco.Selection(range.startLineNumber, range.startColumn, range.startLineNumber, range.startColumn));
      editor.revealRangeInCenterIfOutsideViewport(range);
      editor.focus();
      return;
    }
    await new Promise((r) => setTimeout(r, 50));
  }
}

/** Go to the definition of the symbol at the Lean cursor (any workspace file). */
export async function goToDefinition(): Promise<void> {
  const target = syncedCursor();
  if (!target) {
    notify({ tone: "info", title: "Lean is not ready", body: "Place the cursor in a checked Lean file and try again." });
    return;
  }
  try {
    const locations = await commands.leanDefinition(target.document, target.revision, target.offset);
    const first = locations[0];
    if (!first) {
      notify({ tone: "info", title: "No definition found at the cursor" });
    } else if (!first.path) {
      notify({ tone: "info", title: "Defined outside the workspace", body: `${first.file}:${first.line + 1}` });
    } else {
      await revealInEditor(first.path, first);
    }
  } catch (e) {
    notify({ tone: "warning", title: "Go to definition failed", body: errorMessage(e) });
  }
}

// ----- installation ------------------------------------------------------------------

let installed = false;

export function installLeanMonaco(): void {
  if (installed) return;
  installed = true;
  ensureLanguage(LANGUAGE); // registers the id (shared registry), then refine it here
  monaco.languages.setMonarchTokensProvider(LANGUAGE, leanMonarch);
  monaco.languages.setLanguageConfiguration(LANGUAGE, languageConfiguration);

  monaco.languages.registerCompletionItemProvider(LANGUAGE, {
    triggerCharacters: ["\\"],
    provideCompletionItems(model, position) {
      const before = model.getLineContent(position.lineNumber).slice(0, position.column - 1);
      const match = /\\([^\s\\()[\]{},]*)$/.exec(before);
      if (!match) return { suggestions: [] };
      const range = new monaco.Range(position.lineNumber, match.index + 1, position.lineNumber, position.column);
      return {
        suggestions: abbreviationsWithPrefix(match[1] ?? "")
          .slice(0, 60)
          .map(([name, symbol], i) => ({
            label: `\\${name}`,
            detail: symbol,
            kind: monaco.languages.CompletionItemKind.Text,
            insertText: symbol,
            filterText: `\\${name}`,
            sortText: String(i).padStart(3, "0"),
            range,
          })),
      };
    },
  });

  monaco.languages.registerHoverProvider(LANGUAGE, {
    async provideHover(model, position, token) {
      const handle = handleForModel(model);
      const revision = handle && syncedRevision(handle);
      if (!handle || revision == null) return null;
      try {
        const hover = await commands.leanHover(handle.id, revision, model.getOffsetAt(position));
        if (!hover || token.isCancellationRequested) return null;
        return {
          contents: [{ value: hover.markdown, isTrusted: false, supportHtml: false }],
          range: hover.range ? toMonacoRange(model, hover.range) : undefined,
        };
      } catch {
        return null;
      }
    },
  });

  // Ctrl+Click / F12 for targets that are open in an editor; "Lean: Go to
  // Definition" opens any workspace file.
  monaco.languages.registerDefinitionProvider(LANGUAGE, {
    async provideDefinition(model, position) {
      const handle = handleForModel(model);
      const revision = handle && syncedRevision(handle);
      if (!handle || revision == null) return null;
      try {
        const locations = await commands.leanDefinition(handle.id, revision, model.getOffsetAt(position));
        return locations.flatMap((l) => {
          const target = l.path ? handleForPath(l.path) : undefined;
          if (!target || !l.range) return [];
          return [{ uri: target.model.uri, range: toMonacoRange(target.model, l.range) }];
        });
      } catch {
        return null;
      }
    },
  });

  // Cross-file navigation inside Monaco: show the target document's tab.
  monaco.editor.registerEditorOpener({
    openCodeEditor(_source, resource, selectionOrPosition) {
      const handle = openHandles().find((h) => h.model.uri.toString() === resource.toString());
      if (!handle?.path || !isLeanModel(handle.model)) return false;
      const start =
        selectionOrPosition && "startLineNumber" in selectionOrPosition
          ? { lineNumber: selectionOrPosition.startLineNumber, column: selectionOrPosition.startColumn }
          : selectionOrPosition;
      const offset = start ? handle.model.getOffsetAt(start) : 0;
      void revealInEditor(handle.path, { range: { start: offset, end: offset } });
      return true;
    },
  });

  monaco.editor.onDidCreateEditor(attachEditor);
  for (const editor of monaco.editor.getEditors()) attachEditor(editor);
  onLeanDocument(updateModel);
}
