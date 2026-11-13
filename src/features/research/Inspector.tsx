// Inspector of the selected Research item: provenance, retention state,
// experiments, and graph dependencies over the canonical identities.
import { BookmarkCheck, ExternalLink, Play, Save, Trash2, Archive } from "lucide-react";
import { useEffect, useMemo, useState, useSyncExternalStore } from "react";

import { editors } from "../../app/editors";
import { EDITOR_PAGE, pages } from "../../app/pages";
import { registry } from "../../app/registry";
import type { EntityId } from "../../bindings/EntityId";
import type { ExperimentDetail } from "../../bindings/ExperimentDetail";
import type { RunManifest } from "../../bindings/RunManifest";
import type { SourceInspection } from "../../bindings/SourceInspection";
import { graphCommands } from "../../lib/graphIpc";
import { commands } from "../../lib/ipc";
import { Banner, Button, Checkbox, EmptyState, Section, Select, StatusPill, TextField } from "../../ui/components";
import { fileKey } from "../editor/documentClient";
import { GraphDependencies } from "./GraphInspect";
import { formatBytes, formatClock, runPill } from "./model";
import { PromotionControls, lifecyclePill } from "./Promotion";
import { Provenance } from "./ResearchPanel";
import {
  loadExperiment,
  runExperiment,
  saveExperiment,
  selectItem,
  setResultState,
  useSpace,
  type ResearchItem,
} from "./space";
import { runFile, selectRun, useResearch } from "./store";

const subscribe = (l: () => void) => registry.subscribe(l);
const version = () => registry.getVersion();

function openInEditorPage(path: string) {
  editors.open({ key: fileKey(path), path, title: path.split("/").pop() ?? path });
  pages.show(EDITOR_PAGE);
}

function FileInspector({ path }: { path: string }) {
  const listing = useSpace((s) => s.listing);
  const revision = listing?.graphRevision;
  const file = useMemo(
    () => [...(listing?.sources ?? []), ...(listing?.data ?? []), ...(listing?.notes ?? [])].find((f) => f.path === path),
    [listing, path],
  );
  const script = /\.(py|r|jl|ipy)$/i.test(path);
  return (
    <div data-testid="research-inspector" data-kind="file" data-path={path}>
      <h3 className="exp-rinspector__title">{path.split("/").pop()}</h3>
      <dl className="exp-research__facts">
        <dt>Path</dt>
        <dd>
          <code>{path}</code>
        </dd>
        {file && (
          <>
            <dt>Roles</dt>
            <dd>{file.roles.join(", ") || "—"}</dd>
            {file.sizeBytes !== null && (
              <>
                <dt>Size</dt>
                <dd>{formatBytes(file.sizeBytes)}</dd>
              </>
            )}
          </>
        )}
        {file?.origin && (
          <>
            <dt>Imported</dt>
            <dd data-testid="research-import-origin">
              {file.origin.mode === "copy" ? "Copied" : "Referenced"} from <code>{file.origin.source}</code> on{" "}
              {formatClock(file.origin.importedAtMs)} · sha256 <code>{file.origin.contentHash.slice(0, 12)}…</code>
            </dd>
          </>
        )}
      </dl>
      <div className="exp-rinspector__actions">
        {script && (
          <Button size="sm" variant="primary" icon={Play} onPress={() => void runFile(path)}>
            Run file
          </Button>
        )}
        <Button size="sm" icon={ExternalLink} onPress={() => openInEditorPage(path)}>
          Open in Editor
        </Button>
      </div>
      <DeclaredInFile path={path} revision={revision} />
      {file?.entity ? (
        <GraphDependencies entity={file.entity} revision={revision} />
      ) : (
        <p className="exp-muted">{listing?.graphUnavailable ?? "Not yet in the project graph."}</p>
      )}
    </div>
  );
}

/** Entities the graph binds inside a file (declarations, findings), with extraction coverage. */
function DeclaredInFile({ path, revision }: { path: string; revision: number | null | undefined }) {
  const [inspection, setInspection] = useState<SourceInspection | null>(null);
  useEffect(() => {
    let live = true;
    graphCommands.inspectAtSource({ path, offset: null, revision: null }).then(
      (i) => live && setInspection(i),
      () => live && setInspection(null),
    );
    return () => {
      live = false;
    };
  }, [path, revision]);
  if (!inspection) return null;
  return (
    <Section title={`In this file (${inspection.entities.length})`} collapsible defaultExpanded={inspection.entities.length > 0}>
      <div data-testid="research-in-file" data-coverage={inspection.coverage?.state ?? "unknown"}>
        {inspection.entities.length === 0 ? (
          <p className="exp-muted">No declarations or findings are bound to this file.</p>
        ) : (
          <ul className="exp-graph__list">
            {inspection.entities.map((e) => (
              <li key={e.entity.id}>
                {e.entity.kind} <strong>{e.entity.label}</strong> <span className="exp-muted">({e.binding.state})</span>
              </li>
            ))}
          </ul>
        )}
        <p className="exp-muted">
          Extraction: {inspection.coverage ? inspection.coverage.state : "not indexed yet (coverage unknown)"}
        </p>
      </div>
    </Section>
  );
}

function RunFacts({ runId }: { runId: string }) {
  const run = useResearch((s) => s.runs.find((r) => r.id === runId));
  if (!run) return <p className="exp-muted">Run {runId} is not in the run history of this project.</p>;
  const pill = runPill(run);
  return (
    <>
      <Button size="sm" variant="ghost" onPress={() => void selectRun(run.id)}>
        <StatusPill size="sm" status={pill.status} label={pill.label} /> Show run output
      </Button>
      <Provenance run={run} />
    </>
  );
}

function ResultInspector({ entity }: { entity: EntityId }) {
  const listing = useSpace((s) => s.listing);
  const result = listing?.results.find((r) => r.entity === entity);
  const run = useResearch((s) => s.runs.find((r) => r.id === result?.runId));
  const artifact = run?.outputs.find((a) => a.id === result?.artifactId);
  if (!result) return <EmptyState title="Result not found" description="It may have been removed from the project graph." />;
  const pill = lifecyclePill(result);
  return (
    <div data-testid="research-inspector" data-kind="result" data-entity={entity} data-lifecycle={result.lifecycle}>
      <h3 className="exp-rinspector__title">
        {result.label} <StatusPill size="sm" status={pill.status} label={pill.label} />
      </h3>
      <dl className="exp-research__facts">
        <dt>Path</dt>
        <dd>
          <code>{result.path}</code>
        </dd>
        <dt>Kind</dt>
        <dd>
          {result.kind ?? "output"} · {result.mediaType} · roles {result.roles.join(" + ")}
        </dd>
        {result.contentHash && (
          <>
            <dt>Digest</dt>
            <dd>
              <code title={result.contentHash}>sha256 {result.contentHash.slice(0, 16)}…</code>
            </dd>
          </>
        )}
        <dt>Used by</dt>
        <dd data-testid="research-result-used-by">
          {result.usedBy.length === 0 ? "no manuscript" : result.usedBy.map((u) => u.label).join(", ")}
        </dd>
      </dl>
      <div className="exp-rinspector__actions">
        {result.lifecycle !== "active" && (
          <Button size="sm" icon={BookmarkCheck} onPress={() => void setResultState(entity, "active")}>
            Keep
          </Button>
        )}
        {result.lifecycle !== "archived" && (
          <Button size="sm" icon={Archive} onPress={() => void setResultState(entity, "archived")}>
            Archive
          </Button>
        )}
        {result.lifecycle !== "discarded" && (
          <Button size="sm" icon={Trash2} onPress={() => void setResultState(entity, "discarded")}>
            Discard
          </Button>
        )}
      </div>
      {result.lifecycle === "discarded" && result.usedBy.length > 0 && (
        <Banner tone="info" title="Discarded, still attached">
          Discarding never removes an attachment: {result.usedBy.map((u) => u.label).join(", ")} still uses this file.
        </Banner>
      )}
      {run && artifact && (artifact.kind === "figure" || artifact.kind === "table") && (
        <PromotionControls run={run} artifact={artifact} />
      )}
      {result.runId && (
        <Section title="Run" collapsible defaultExpanded>
          <RunFacts runId={result.runId} />
        </Section>
      )}
      <GraphDependencies entity={entity} revision={listing?.graphRevision} />
    </div>
  );
}

function ExperimentEditor({ entity }: { entity: EntityId | null }) {
  const listing = useSpace((s) => s.listing);
  const loaded = useSpace((s) => s.experiment);
  const detail: ExperimentDetail | null = entity && loaded?.summary.entity === entity ? loaded : null;
  const [title, setTitle] = useState("");
  const [objective, setObjective] = useState("");
  const [procedure, setProcedure] = useState<string | null>(null);
  const [inputs, setInputs] = useState<string[]>([]);
  const [outcomes, setOutcomes] = useState<string[]>([]);
  const [saving, setSaving] = useState(false);
  // Attempts follow run history: reload when a run finishes.
  const finishedRuns = useResearch((s) => s.runs.filter((r) => r.status.kind !== "running").length);
  useEffect(() => {
    if (entity) void loadExperiment(entity);
  }, [entity, finishedRuns]);

  useEffect(() => {
    setTitle(detail?.summary.label ?? "");
    setObjective(detail?.summary.objective ?? "");
    setProcedure(detail?.procedure?.path ?? null);
    setInputs(detail?.inputs.map((i) => i.path) ?? []);
    setOutcomes(detail?.outcomes.map((o) => o.entity) ?? []);
  }, [detail]);

  if (entity && !detail) return <p className="exp-muted">Loading the experiment…</p>;
  const scripts = (listing?.sources ?? []).filter((f) => f.roles.includes("script"));
  const inputChoices = [...(listing?.data ?? []), ...(listing?.sources ?? []).filter((f) => !f.roles.includes("script"))];
  const toggle = (list: string[], value: string, on: boolean) => (on ? [...list, value] : list.filter((v) => v !== value));
  const save = async () => {
    setSaving(true);
    await saveExperiment({
      entity,
      expectedRevision: detail?.summary.revision ?? null,
      title,
      objective,
      procedure,
      inputs,
      outcomes,
    });
    setSaving(false);
  };
  return (
    <div data-testid="research-inspector" data-kind="experiment" data-entity={entity ?? "new"}>
      <h3 className="exp-rinspector__title">{entity ? "Experiment" : "New experiment"}</h3>
      <p className="exp-muted">
        An experiment records an objective and procedure, its declared inputs, and its retained outcomes. Past runs are
        attempts, not experiments.
      </p>
      <TextField label="Title" value={title} onChange={setTitle} />
      <TextField label="Objective" value={objective} onChange={setObjective} />
      <Select<string>
        label="Procedure (script)"
        placeholder={scripts.length ? "Choose a script" : "Create a script in Sources first"}
        options={scripts.map((s) => ({ id: s.path, label: s.name }))}
        value={procedure}
        onChange={setProcedure}
      />
      <Section title="Declared inputs">
        {inputChoices.length === 0 ? (
          <p className="exp-muted">No data in the research space yet.</p>
        ) : (
          inputChoices.map((f) => (
            <Checkbox key={f.path} isSelected={inputs.includes(f.path)} onChange={(on) => setInputs((l) => toggle(l, f.path, on))}>
              {f.name}
            </Checkbox>
          ))
        )}
      </Section>
      <Section title="Outcomes (retained results)">
        {(listing?.results ?? []).length === 0 ? (
          <p className="exp-muted">Keep a figure or table from a run to add it here.</p>
        ) : (
          (listing?.results ?? []).map((r) => (
            <Checkbox key={r.entity} isSelected={outcomes.includes(r.entity)} onChange={(on) => setOutcomes((l) => toggle(l, r.entity, on))}>
              {r.label}
            </Checkbox>
          ))
        )}
      </Section>
      <div className="exp-rinspector__actions">
        <Button size="sm" variant="primary" icon={Save} isPending={saving} isDisabled={!title.trim()} onPress={() => void save()}>
          Save experiment
        </Button>
        {detail?.procedure && (
          <Button size="sm" icon={Play} onPress={() => void runExperiment(detail)}>
            Run procedure
          </Button>
        )}
      </div>
      {detail && (
        <>
          <Section title={`Attempts (${detail.attempts.length})`}>
            {detail.attempts.length === 0 ? (
              <p className="exp-muted">No run of the procedure yet.</p>
            ) : (
              <ul className="exp-graph__list" data-testid="research-attempts">
                {detail.attempts.map((run) => {
                  const pill = runPill(run);
                  return (
                    <li key={run.id}>
                      <Button size="sm" variant="ghost" onPress={() => selectItem({ kind: "run", runId: run.id })}>
                        <StatusPill size="sm" status={pill.status} label={pill.label} /> {formatClock(run.startedAtMs)}
                      </Button>
                    </li>
                  );
                })}
              </ul>
            )}
          </Section>
          <GraphDependencies entity={detail.summary.entity} revision={detail.graphRevision} />
        </>
      )}
    </div>
  );
}

function WorkflowInspector({ entity }: { entity: EntityId }) {
  useSyncExternalStore(subscribe, version);
  const listing = useSpace((s) => s.listing);
  const workflow = listing?.workflows.find((w) => w.entity === entity);
  const resource = { key: `workflow:${entity}`, title: workflow?.label ?? "Workflow" };
  const editor = registry.editorFor(resource);
  return (
    <div data-testid="research-inspector" data-kind="workflow" data-entity={entity}>
      <h3 className="exp-rinspector__title">{workflow?.label ?? "Workflow"}</h3>
      {editor ? (
        <Button
          size="sm"
          variant="primary"
          onPress={() => {
            editors.open(resource);
            pages.show(EDITOR_PAGE);
          }}
        >
          Open in the workflow editor
        </Button>
      ) : (
        <EmptyState
          fill={false}
          title="The workflow editor is not available"
          description="Saved workflow definitions are listed here read-only."
        />
      )}
      <GraphDependencies entity={entity} revision={listing?.graphRevision} />
    </div>
  );
}

function ReferenceInspector({ entity }: { entity: EntityId }) {
  const listing = useSpace((s) => s.listing);
  const reference = listing?.references.find((r) => r.entity === entity);
  if (!reference) return <EmptyState title="Reference not found" />;
  return (
    <div data-testid="research-inspector" data-kind="reference" data-entity={entity}>
      <h3 className="exp-rinspector__title">{reference.label}</h3>
      <dl className="exp-research__facts">
        <dt>Location</dt>
        <dd>
          <code>{reference.origin.source}</code>
        </dd>
        <dt>When referenced</dt>
        <dd>
          {formatClock(reference.origin.importedAtMs)} · {formatBytes(reference.origin.sizeBytes)} · sha256{" "}
          <code>{reference.origin.contentHash.slice(0, 12)}…</code>
        </dd>
      </dl>
      <p className="exp-muted">A read-only reference: Exposition never copies, crawls, or runs it.</p>
      <GraphDependencies entity={entity} revision={listing?.graphRevision} />
    </div>
  );
}

function RunInspector({ runId }: { runId: string }) {
  const [manifest, setManifest] = useState<RunManifest | null>(null);
  useEffect(() => {
    let live = true;
    commands.researchRun(runId).then(
      (m) => live && setManifest(m),
      () => live && setManifest(null),
    );
    return () => {
      live = false;
    };
  }, [runId]);
  return (
    <div data-testid="research-inspector" data-kind="run" data-run={runId}>
      <h3 className="exp-rinspector__title">Run {runId}</h3>
      <p className="exp-muted">
        Runs are operational history: they stay inspectable here and become part of the graph only through results you keep.
      </p>
      {manifest && <p>{manifest.record.outputs.length} outputs</p>}
      <RunFacts runId={runId} />
    </div>
  );
}

export function ResearchInspector({ item }: { item: ResearchItem | null }) {
  if (!item) {
    return (
      <EmptyState
        fill={false}
        title="Nothing selected"
        description="Select a source, dataset, experiment, or result to see its provenance and graph dependencies."
      />
    );
  }
  switch (item.kind) {
    case "file":
      return <FileInspector key={item.path} path={item.path} />;
    case "result":
      return <ResultInspector key={item.entity} entity={item.entity} />;
    case "experiment":
      return <ExperimentEditor key={item.entity ?? "new"} entity={item.entity} />;
    case "workflow":
      return <WorkflowInspector key={item.entity} entity={item.entity} />;
    case "reference":
      return <ReferenceInspector key={item.entity} entity={item.entity} />;
    case "run":
      return <RunInspector key={item.runId} runId={item.runId} />;
  }
}
