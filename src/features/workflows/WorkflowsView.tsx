// Workflow drafts of the open project (editor tab `workflows:`): saved drafts,
// new drafts from the workflow presets, and strict imports. Nothing here runs.
import { FileUp, Plus } from "lucide-react";
import { useEffect, useId, useState } from "react";

import { editors } from "../../app/editors";
import type { EditorProps } from "../../app/registry";
import type { WorkflowPresetInfo } from "../../bindings/WorkflowPresetInfo";
import type { WorkflowSummary } from "../../bindings/WorkflowSummary";
import { errorMessage } from "../../lib/ipc";
import { Banner, Button, EmptyState, ErrorState, StatusPill } from "../../ui/components";
import { useWorkspace } from "../workspace/store";
import { workflowCommands } from "./ipc";
import { workflowKey } from "./model";
import { importDraft, newDraftFromPreset } from "./actions";

export function WorkflowsView(_props: EditorProps) {
  const [drafts, setDrafts] = useState<WorkflowSummary[] | null>(null);
  const [presets, setPresets] = useState<WorkflowPresetInfo[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [text, setText] = useState("");
  const [importError, setImportError] = useState<string | null>(null);
  const id = useId();
  const root = useWorkspace((s) => s.info?.root ?? null);

  const refresh = async () => {
    try {
      const [list, p] = await Promise.all([workflowCommands.list(), workflowCommands.presets()]);
      setDrafts(list);
      setPresets(p);
      setError(null);
    } catch (e) {
      setError(errorMessage(e));
    }
  };
  useEffect(() => {
    void refresh();
  }, [root]);

  if (error) return <ErrorState title="Workflow drafts are unavailable" description={error} actions={<Button onPress={() => void refresh()}>Retry</Button>} />;
  return (
    <div className="xp-wf" data-testid="workflows-view">
      <div className="xp-wf__page">
        <header className="xp-wf__header">
          <div className="xp-wf__grow">
            <h2 className="xp-wf__title">Workflow drafts</h2>
            <p className="xp-wf__muted">Saved procedures for this project. Drafts are validated against the strict workflow contract; running them arrives in a later version.</p>
          </div>
        </header>
        <section className="xp-wf__group" aria-label="New draft">
          <h3 className="xp-wf__group-title">New draft from a workflow preset</h3>
          <ul className="xp-wf__cards">
            {presets.map((p) => (
              <li key={p.id} className="xp-wf__card">
                <strong>{p.title}</strong>
                <p className="xp-wf__muted">{p.description}</p>
                <div>
                  <Button icon={Plus} onPress={() => void newDraftFromPreset(p.id)} data-testid={`workflow-new-${p.id}`}>
                    New {p.title} Draft
                  </Button>
                </div>
              </li>
            ))}
          </ul>
        </section>
        <section className="xp-wf__group" aria-label="Saved drafts">
          <h3 className="xp-wf__group-title">Saved drafts</h3>
          {drafts && drafts.length === 0 && <EmptyState fill={false} title="No workflow drafts yet" description="Create one from a preset above, or import a definition." />}
          <ul className="xp-wf__cards" data-testid="workflow-drafts">
            {drafts?.map((d) => (
              <li key={d.id} className="xp-wf__card xp-wf__row" data-workflow={d.id}>
                <div className="xp-wf__grow">
                  <strong>{d.title}</strong>
                  <p className="xp-wf__muted">
                    Revision {d.revision}
                    {d.imported ? " · imported" : ""} · not runnable in this version
                  </p>
                </div>
                <StatusPill status={d.state === "wellFormed" ? "idle" : "warning"} label={d.state === "wellFormed" ? "Well-formed draft" : "Incomplete draft"} />
                <Button onPress={() => editors.open({ key: workflowKey(d.id), title: d.title })}>Open</Button>
              </li>
            ))}
          </ul>
        </section>
        <section className="xp-wf__group" aria-label="Import">
          <h3 className="xp-wf__group-title">Import a definition</h3>
          <p className="xp-wf__muted">Paste an exposition.workflow/1 definition. It is checked strictly, saved as a new draft with a new identity, never trusted, and never run.</p>
          <div className="ui-field">
            <label className="ui-field__label" htmlFor={id}>
              Definition (JSON)
            </label>
            <textarea id={id} className="ui-input xp-wf__textarea" data-code rows={6} value={text} onChange={(e) => setText(e.target.value)} spellCheck={false} data-testid="workflow-import-text" />
          </div>
          {importError && (
            <Banner tone="danger" title="Not imported" live>
              {importError}
            </Banner>
          )}
          <div className="xp-wf__row">
            <Button
              icon={FileUp}
              isDisabled={!text.trim()}
              onPress={async () => {
                const result = await importDraft(text);
                if (result.ok) {
                  setText("");
                  setImportError(null);
                  void refresh();
                } else setImportError(result.error);
              }}
              data-testid="workflow-import"
            >
              Import Draft
            </Button>
          </div>
        </section>
      </div>
    </div>
  );
}
