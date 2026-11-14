// Workflows of the open project (editor tab `workflows:`): saved drafts, new
// drafts from the workflow presets, strict imports, and recent runs. Nothing
// here starts a run: a run starts only from a reviewed plan in the editor.
import { Bell, BellOff, BellRing, FileUp, Plus } from "lucide-react";
import { useEffect, useState } from "react";

import { editors } from "../../app/editors";
import type { EditorProps } from "../../app/registry";
import type { WorkflowPresetInfo } from "../../bindings/WorkflowPresetInfo";
import type { WorkflowSummary } from "../../bindings/WorkflowSummary";
import { errorMessage } from "../../lib/ipc";
import {
  Badge,
  Banner,
  Button,
  EmptyState,
  ErrorState,
  ListRow,
  Menu,
  MenuItem,
  MenuTrigger,
  PageHeader,
  Section,
  StateSizeProvider,
  StatusText,
  TextArea,
} from "../../ui/components";
import { onProjectOp } from "../operations/client";
import { useOpList } from "../operations/useOpList";
import { useWorkspace } from "../workspace/store";
import type { WorkflowTrigger } from "./contract";
import { workflowCommands, workflowTriggerCommands } from "./ipc";
import { workflowKey } from "./model";
import { importBundle, importDraft, newDraftFromPreset } from "./actions";
import { RunHistory } from "./RunView";
import { workflowActions } from "./workflowOps";

export function WorkflowsView(_props: EditorProps) {
  const [drafts, setDrafts] = useState<WorkflowSummary[] | null>(null);
  const [presets, setPresets] = useState<WorkflowPresetInfo[]>([]);
  const [triggers, setTriggers] = useState<WorkflowTrigger[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [text, setText] = useState("");
  const [importError, setImportError] = useState<string | null>(null);
  const root = useWorkspace((s) => s.info?.root ?? null);

  const refresh = async () => {
    try {
      const [list, p] = await Promise.all([workflowCommands.list(), workflowCommands.presets()]);
      setDrafts(list);
      setPresets(p);
      setError(null);
      // Trigger state is supplementary here (the editor's Triggers section is the place to act).
      setTriggers(await workflowTriggerCommands.list(null).catch(() => []));
    } catch (e) {
      setError(errorMessage(e));
    }
  };
  useEffect(() => {
    void refresh();
  }, [root]);
  // Deletions, copies and restores (here, in Storage & History, or by an agent).
  useEffect(() => onProjectOp(() => void refresh()), []);
  const list = useOpList({
    id: "workflows",
    label: "Workflow actions",
    items: drafts ?? [],
    keyOf: (d) => d.id,
    actionsFor: (selected) => workflowActions(selected),
  });

  if (error) return <ErrorState title="Workflow drafts are unavailable" description={error} actions={<Button onPress={() => void refresh()}>Retry</Button>} />;
  return (
    <div className="xp-wf" data-testid="workflows-view">
      <PageHeader
        title="Workflows"
        actions={
          <>
            <MenuTrigger>
              <Button icon={Plus} isDisabled={presets.length === 0} data-testid="workflow-new">
                New Draft
              </Button>
              <Menu aria-label="New draft from a workflow preset" onAction={(key) => void newDraftFromPreset(String(key))}>
                {presets.map((p) => (
                  <MenuItem key={p.id} id={p.id} description={p.description} data-testid={`workflow-new-${p.id}`}>
                    {`New ${p.title} Draft`}
                  </MenuItem>
                ))}
              </Menu>
            </MenuTrigger>
            <Button
              icon={FileUp}
              onPress={async () => {
                const result = await importBundle();
                if (!result) return;
                if (result.ok) {
                  setImportError(null);
                  void refresh();
                } else setImportError(result.error);
              }}
              data-testid="workflow-import-bundle"
            >
              Import Bundle…
            </Button>
          </>
        }
      />
      <div className="xp-wf__body">
        <div className="xp-wf__page">
          <StateSizeProvider value="compact">
            <div className="xp-wf__lead">
              <p className="ui-meta">
                Saved procedures for this project, validated against the strict workflow contract. A saved, well-formed revision runs
                only after you review its full plan and press Start run; saving, importing, or an agent proposal never runs anything.
              </p>
              {importError && (
                <Banner tone="danger" title="Not imported" live>
                  {importError}
                </Banner>
              )}
            </div>
            <Section title="Saved drafts">
              {drafts && drafts.length === 0 && (
                <EmptyState title="No workflow drafts yet" description="Create one from a preset (New Draft), or import a definition." />
              )}
              <div ref={list.ref} className="xp-ops-surface">
                <div
                  className="xp-wf-list"
                  role="list"
                  data-testid="workflow-drafts"
                  aria-label="Saved drafts (select with click or Space; Ctrl+click adds)"
                >
                  {drafts?.map((d) => (
                    <ListRow
                      key={d.id}
                      {...list.itemProps(d.id)}
                      role="listitem"
                      data-workflow={d.id}
                      aria-label={d.title}
                      title={<strong>{d.title}</strong>}
                      meta={
                        <>
                          <StatusText
                            status={d.state === "wellFormed" ? "idle" : "warning"}
                            label={d.state === "wellFormed" ? "Well-formed draft" : "Incomplete draft"}
                          />
                          <span>
                            Revision {d.revision}
                            {d.imported ? " · imported" : ""} · {d.runnable ? "can be reviewed and run" : "cannot run yet"}
                          </span>
                          {triggers.some((t) => t.workflow === d.id && t.state.kind === "suspended") ? (
                            <Badge tone="warning" icon={BellOff} title="A trigger of this workflow is suspended until you review its standing plan again">
                              Trigger suspended
                            </Badge>
                          ) : triggers.some((t) => t.workflow === d.id && t.state.kind === "active") ? (
                            <Badge tone="info" icon={Bell} title="Runs start by themselves under an approved standing plan">
                              Trigger on
                            </Badge>
                          ) : (
                            triggers.some((t) => t.workflow === d.id && t.state.kind === "disabled") && (
                              <Badge
                                icon={BellRing}
                                dashed
                                title="Triggers stay off until you enable one again in the workflow's Triggers section"
                                data-testid={`workflow-trigger-off-${d.id}`}
                              >
                                Trigger off
                              </Badge>
                            )
                          )}
                          {d.origin.kind === "agent" && (
                            <Badge tone="accent" dashed title={`Proposed by agent task ${d.origin.taskId}; review it before running`}>
                              Agent proposal
                            </Badge>
                          )}
                        </>
                      }
                      trailing={
                        <>
                          <Button size="sm" variant="ghost" onPress={() => editors.open({ key: workflowKey(d.id), title: d.title })} data-testid={`workflow-open-${d.id}`}>
                            Open
                          </Button>
                          {list.moreButton(d.id, d.title)}
                        </>
                      }
                    />
                  ))}
                </div>
                {list.contextMenu}
              </div>
            </Section>
            <RunHistory workflow={null} title="Recent runs" />
            <Section title="Import a definition">
              <div className="xp-wf__stack">
                <p className="ui-meta">
                  Import a workflow bundle file (a workflow with the workflows it runs), or paste an exposition.workflow/1 or /2
                  definition. It is checked strictly, saved as new drafts with new identities, never trusted, and never run; triggers,
                  approvals, and trust are never imported.
                </p>
                <TextArea label="Definition (JSON)" code rows={6} value={text} onChange={setText} testId="workflow-import-text" />
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
              </div>
            </Section>
          </StateSizeProvider>
        </div>
      </div>
    </div>
  );
}
