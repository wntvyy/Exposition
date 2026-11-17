// The Equation panel (auxiliary view): the target line, the structural
// editor, templates, an approximate KaTeX preview, notices (requirements,
// unsupported or kept-as-written parts, conflicts, another document), Apply /
// Cancel, and the symbol browser. Shared by the source and page surfaces.
import { useEffect, useMemo, useRef, useSyncExternalStore } from "react";

import { Banner, Button, Checkbox, EmptyState, SegmentedControl, Section } from "../../../ui/components";
import { MathTeX } from "../../../ui/components/MarkdownMath";
import { editGrid, insertSymbol, insertTemplate, languageForPath, type CatalogEntry } from "../core";
import { requirementLabel, type Draft, type EquationPanelStore } from "./draft";
import { MathEditor } from "./MathEditor";
import { SymbolBrowser } from "./SymbolBrowser";
import { TemplateBar } from "./TemplateBar";
import { inGrid, LANGUAGE_LABELS, nodesOnPath } from "./tree";
import "./panel.css";

export interface EquationViewProps {
  store: EquationPanelStore;
  /** The active editor tab's document (null without one). */
  activePath: string | null;
  onEditInSource: (path: string, range: { start: number; end: number } | null) => void;
}

function targetTitle(draft: Draft): string {
  if (draft.kind === "edit") return `Editing equation at line ${draft.line}`;
  if (draft.context === "math") return `Inserting into the equation at line ${draft.line}`;
  return `New ${draft.display ? "display" : "inline"} equation at line ${draft.line}`;
}

function conflictText(draft: Draft): string {
  switch (draft.conflict?.kind) {
    case "closed":
      return `${draft.name} was closed. Nothing was written; the draft is kept.`;
    case "replaced":
      return `${draft.name} was reloaded or refreshed. Nothing was written; the draft is kept.`;
    default:
      return draft.kind === "edit"
        ? "The equation was changed in the document after this draft opened. Nothing was written; the draft is kept."
        : "The text at the insertion point was changed after this draft opened. Nothing was written; the draft is kept.";
  }
}

export function EquationView({ store, activePath, onEditInSource }: EquationViewProps) {
  const { draft, notice } = useSyncExternalStore(store.subscribe, store.getState);
  const editorRef = useRef<HTMLDivElement>(null);
  const analysis = useMemo(() => store.analyze(draft), [store, draft]);
  const activeLanguage = activePath ? languageForPath(activePath) : null;
  const language = draft?.language ?? activeLanguage;

  // A newly opened (or re-targeted) draft takes keyboard focus.
  const serial = draft?.serial;
  useEffect(() => {
    if (serial !== undefined) editorRef.current?.focus();
  }, [serial]);

  const onInsertSymbol = (entry: CatalogEntry, keyboard: boolean) => {
    if (store.getState().draft) {
      store.edit((s) => insertSymbol(s, entry.id));
      if (!keyboard) editorRef.current?.focus();
    } else {
      void store.insertSymbolAtCaret(entry.id);
    }
  };

  const noticeBanner = notice && (
    <Banner tone={notice.tone} live data-testid="equation-notice">
      {notice.text}
    </Banner>
  );

  const symbols = language && (
    <Section title="Symbols" collapsible data-testid="equation-symbols-section">
      <SymbolBrowser language={language} onInsert={onInsertSymbol} />
    </Section>
  );

  if (!draft || !analysis) {
    return (
      <div className="xp-eq" data-testid="equation-view" data-state="idle">
        {noticeBanner}
        <EmptyState
          title="No equation open"
          description={
            activeLanguage
              ? "Insert one at the caret of the source or page, or edit the equation the caret is in. Symbols below go in at the caret."
              : "Open a LaTeX, Typst, or Markdown document to insert or edit equations."
          }
          actions={
            activeLanguage ? (
              <>
                <Button size="sm" onPress={() => void store.open("inline")} data-testid="equation-insert-inline">
                  Insert Inline
                </Button>
                <Button size="sm" onPress={() => void store.open("display")} data-testid="equation-insert-display">
                  Insert Display
                </Button>
                <Button size="sm" onPress={() => void store.open("edit")} data-testid="equation-edit">
                  Edit Equation
                </Button>
              </>
            ) : undefined
          }
        />
        {symbols}
      </div>
    );
  }

  const unsupported = analysis.serialized?.unsupported ?? [];
  const blocked = draft.conflict !== null || unsupported.length > 0 || analysis.error !== null;
  const caretNodes = nodesOnPath(draft.editor.root, draft.editor.caret.path);
  const gridNode = [...caretNodes].reverse().find((n) => n.kind === "matrix" || n.kind === "cases" || n.kind === "aligned");
  const enterApplies = !blocked && !inGrid(draft.editor.root, draft.editor.caret.path);
  const otherDocument = activePath !== draft.path;
  const currentEquation = draft.conflict?.kind === "changed" ? store.currentEquation() : null;
  const facts = [draft.name, LANGUAGE_LABELS[draft.language], draft.surface === "page" ? "from the page" : "from the source"];
  if (draft.kind === "edit") facts.push(draft.display ? "display" : "inline");
  const missingLabels = analysis.missing.map((m) => requirementLabel(m.requirement)).join(", ");
  const addable = analysis.missing.some((m) => m.addable);

  return (
    <div
      className="xp-eq"
      data-testid="equation-view"
      data-state="draft"
      data-kind={draft.kind}
      data-context={draft.context}
      data-language={draft.language}
      data-path={draft.path}
      data-conflict={draft.conflict?.kind ?? ""}
      data-unchanged={analysis.unchanged ? "true" : "false"}
    >
      <div className="xp-eq__target" data-testid="equation-target">
        <p className="xp-eq__title">{targetTitle(draft)}</p>
        <p className="ui-meta">{facts.join(" · ")}</p>
        {draft.kind === "insert" && draft.context === "text" && (
          <SegmentedControl
            aria-label="Equation kind"
            value={draft.display ? "display" : "inline"}
            onChange={(value) => store.setDisplay(value === "display")}
            options={[
              { id: "inline", label: "Inline" },
              { id: "display", label: "Display" },
            ]}
            data-testid="equation-display-toggle"
          />
        )}
      </div>

      {noticeBanner}

      {draft.conflict && (
        <Banner
          tone="warning"
          title="The target changed"
          live
          data-testid="equation-conflict"
          actions={
            <>
              {currentEquation && (
                <Button size="sm" onPress={() => store.retargetToCurrentEquation()} data-testid="equation-retarget-equation">
                  Use current equation
                </Button>
              )}
              <Button size="sm" onPress={() => void store.retargetToCaret()} data-testid="equation-retarget-caret">
                Move to caret
              </Button>
              <Button size="sm" onPress={() => void store.copyDraft()} data-testid="equation-copy">
                Copy draft
              </Button>
            </>
          }
        >
          {conflictText(draft)}
        </Banner>
      )}

      {otherDocument && !draft.conflict && (
        <Banner
          tone="info"
          title={`This draft belongs to ${draft.name}`}
          data-testid="equation-other-document"
          actions={
            <Button size="sm" onPress={() => void store.retargetToCaret()} data-testid="equation-move">
              Move to caret
            </Button>
          }
        >
          Apply writes to {draft.name} at line {draft.line}. Cancel discards it.
        </Banner>
      )}

      {analysis.opaque.length > 0 && (
        <Banner
          tone="info"
          title="Kept exactly as written"
          data-testid="equation-opaque"
          actions={
            <Button size="sm" onPress={() => onEditInSource(draft.path, store.targetRange())} data-testid="equation-edit-in-source">
              Edit in Source
            </Button>
          }
        >
          Parts of this equation cannot be edited here; they stay unchanged in the source.
          <ul className="xp-eq__list">
            {[...new Set(analysis.opaque)].map((reason) => (
              <li key={reason}>{reason}</li>
            ))}
          </ul>
        </Banner>
      )}

      {unsupported.length > 0 && (
        <Banner tone="warning" title={`Not available in ${LANGUAGE_LABELS[draft.language]}`} data-testid="equation-unsupported">
          Remove these parts to apply:
          <ul className="xp-eq__list">
            {unsupported.map((u) => (
              <li key={`${u.what}:${u.reason}`}>
                {u.what} — {u.reason}
              </li>
            ))}
          </ul>
        </Banner>
      )}

      {analysis.missing.length > 0 && (
        <Banner tone="info" title={`Needs ${missingLabels}`} data-testid="equation-requirement">
          {addable ? (
            <Checkbox
              isSelected={draft.addRequirements}
              onChange={(selected) => store.setAddRequirements(selected)}
              data-testid="equation-add-requirement"
            >
              Add {missingLabels} to the preamble in the same edit
            </Checkbox>
          ) : (
            "It cannot be added automatically here; add it to the preamble yourself."
          )}
        </Banner>
      )}

      {analysis.error && (
        <Banner tone="danger" title="This draft cannot be written" data-testid="equation-error">
          {analysis.error}
        </Banner>
      )}

      <MathEditor
        ref={editorRef}
        state={draft.editor}
        onEdit={(op) => store.edit(op)}
        onApply={() => void store.apply()}
        onCancel={() => store.cancel()}
        onUndo={() => store.undo()}
        onRedo={() => store.redo()}
        enterApplies={enterApplies}
        aria-label={targetTitle(draft)}
      />

      <TemplateBar
        grid={gridNode ? (gridNode.kind as "matrix" | "cases" | "aligned") : null}
        onTemplate={(template, options) => {
          store.edit((s) => insertTemplate(s, template, options));
          editorRef.current?.focus();
        }}
        onGrid={(edit) => {
          store.edit((s) => editGrid(s, edit));
          editorRef.current?.focus();
        }}
      />

      <Section title="Preview" data-testid="equation-preview">
        <p className="ui-meta xp-eq__preview-note">Approximate. The compiled document is authoritative.</p>
        {analysis.preview ? <MathTeX tex={analysis.preview} display /> : <p className="ui-meta">No preview.</p>}
      </Section>

      <div className="xp-eq__actions">
        <Button variant="primary" isDisabled={blocked} onPress={() => void store.apply()} data-testid="equation-apply">
          Apply
        </Button>
        <Button onPress={() => store.cancel()} data-testid="equation-cancel">
          Cancel
        </Button>
        {analysis.unchanged && <span className="ui-meta">{draft.kind === "edit" ? "No changes" : "Empty"}</span>}
      </div>

      {symbols}
    </div>
  );
}
