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
import {
  Banner,
  Button,
  Checkbox,
  EmptyState,
  InspectorBody,
  InspectorHeader,
  KeyValueList,
  Section,
  Select,
  StatusPill,
  StatusText,
  TextField,
  type KeyValueItem,
} from "../../ui/components";
import { fileKey } from "../editor/documentClient";
import { GraphDependencies } from "./GraphInspect";
import { formatBytes, formatClock, isActiveRun, runPill } from "./model";
import { PromotionControls, lifecyclePill } from "./Promotion";
import { Provenance, RunRow, useNow } from "./ResearchPanel";
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
  const facts: KeyValueItem[] = [{ key: "path", label: "Path", value: path, code: true }];
  if (file) {
    facts.push({ key: "roles", label: "Roles", value: file.roles.join(", ") || "—" });
    if (file.sizeBytes !== null) facts.push({ key: "size", label: "Size", value: formatBytes(file.sizeBytes) });
  }
  if (file?.origin) {
    const origin = file.origin;
    facts.push({
      key: "imported",
      label: "Imported",
      valueProps: { "data-testid": "research-import-origin" },
      value: (
        <>
          {origin.mode === "copy" ? "Copied" : "Referenced"} from <code className="ui-code">{origin.source}</code> on{" "}
          {formatClock(origin.importedAtMs)} · sha256 <code className="ui-code">{origin.contentHash.slice(0, 12)}…</code>
        </>
      ),
    });
  }
  return (
    <div className="exp-rinspector" data-testid="research-inspector" data-kind="file" data-path={path}>
      <InspectorHeader
        title={path.split("/").pop()}
        actions={
          <>
            {script && (
              <Button size="sm" icon={Play} onPress={() => void runFile(path)}>
                Run file
              </Button>
            )}
            <Button size="sm" icon={ExternalLink} onPress={() => openInEditorPage(path)}>
              Open in Editor
            </Button>
          </>
        }
      />
      <div className="exp-rinspector__facts">
        <KeyValueList items={facts} />
      </div>
      <InspectorBody>
        <DeclaredInFile path={path} revision={revision} />
        {file?.entity ? (
          <GraphDependencies entity={file.entity} revision={revision} />
        ) : (
          <p className="ui-meta exp-rinspector__note">{listing?.graphUnavailable ?? "Not yet in the project graph."}</p>
        )}
      </InspectorBody>
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
    <Section title="In this file" count={inspection.entities.length} collapsible defaultExpanded={inspection.entities.length > 0}>
      <div className="exp-rinspector__stack" data-testid="research-in-file" data-coverage={inspection.coverage?.state ?? "unknown"}>
        {inspection.entities.length === 0 ? (
          <p className="ui-meta">No declarations or findings are bound to this file.</p>
        ) : (
          <ul className="exp-graph__list">
            {inspection.entities.map((e) => (
              <li key={e.entity.id}>
                {e.entity.kind} <span className="exp-graph__name">{e.entity.label}</span>{" "}
                <span className="ui-meta">({e.binding.state})</span>
              </li>
            ))}
          </ul>
        )}
        <p className="ui-meta">
          Extraction: {inspection.coverage ? inspection.coverage.state : "not indexed yet (coverage unknown)"}
        </p>
      </div>
    </Section>
  );
}

/** The run behind an item (its state and a way to show its output), then its provenance. */
function RunSections({ runId }: { runId: string }) {
  const run = useResearch((s) => s.runs.find((r) => r.id === runId));
  if (!run) {
    return (
      <Section title="Run">
        <p className="ui-meta">Run {runId} is not in the run history of this project.</p>
      </Section>
    );
  }
  const pill = runPill(run);
  return (
    <>
      <Section title="Run">
        <div className="exp-rinspector__run">
          <StatusText status={pill.status} label={pill.label} />
          <Button size="sm" variant="ghost" onPress={() => void selectRun(run.id)}>
            Show run output
          </Button>
        </div>
      </Section>
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
  const facts: KeyValueItem[] = [
    { key: "path", label: "Path", value: result.path, code: true },
    {
      key: "kind",
      label: "Kind",
      value: `${result.kind ?? "output"} · ${result.mediaType} · roles ${result.roles.join(" + ")}`,
    },
  ];
  if (result.contentHash) {
    facts.push({
      key: "digest",
      label: "Digest",
      value: (
        <code className="ui-code" title={result.contentHash}>
          sha256 {result.contentHash.slice(0, 16)}…
        </code>
      ),
    });
  }
  facts.push({
    key: "usedBy",
    label: "Used by",
    valueProps: { "data-testid": "research-result-used-by" },
    value: result.usedBy.length === 0 ? "no manuscript" : result.usedBy.map((u) => u.label).join(", "),
  });
  return (
    <div className="exp-rinspector" data-testid="research-inspector" data-kind="result" data-entity={entity} data-lifecycle={result.lifecycle}>
      <InspectorHeader
        title={result.label}
        meta={<StatusPill size="sm" status={pill.status} label={pill.label} />}
        actions={
          <>
            {result.lifecycle !== "active" && (
              <Button size="sm" icon={BookmarkCheck} onPress={() => void setResultState(entity, "active")}>
                Keep
              </Button>
            )}
            {result.lifecycle !== "archived" && (
              <Button size="sm" variant="ghost" icon={Archive} onPress={() => void setResultState(entity, "archived")}>
                Archive
              </Button>
            )}
            {result.lifecycle !== "discarded" && (
              <Button size="sm" variant="ghost" icon={Trash2} onPress={() => void setResultState(entity, "discarded")}>
                Discard
              </Button>
            )}
          </>
        }
      />
      <div className="exp-rinspector__facts">
        <KeyValueList items={facts} />
        {result.lifecycle === "discarded" && result.usedBy.length > 0 && (
          <Banner tone="info" title="Discarded, still attached">
            Discarding never removes an attachment: {result.usedBy.map((u) => u.label).join(", ")} still uses this file.
          </Banner>
        )}
        {run && artifact && (artifact.kind === "figure" || artifact.kind === "table") && (
          <PromotionControls run={run} artifact={artifact} />
        )}
      </div>
      <InspectorBody>
        {result.runId && <RunSections runId={result.runId} />}
        <GraphDependencies entity={entity} revision={listing?.graphRevision} />
      </InspectorBody>
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
  const now = useNow(Boolean(detail?.attempts.some(isActiveRun)));
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

  if (entity && !detail) return <p className="ui-meta exp-rinspector__note">Loading the experiment…</p>;
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
    <div className="exp-rinspector" data-testid="research-inspector" data-kind="experiment" data-entity={entity ?? "new"}>
      <InspectorHeader title={entity ? "Experiment" : "New experiment"} />
      <div className="exp-rinspector__facts">
        <p className="ui-meta">
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
        <fieldset className="exp-rinspector__group">
          <legend className="ui-group-label">Declared inputs</legend>
          {inputChoices.length === 0 ? (
            <p className="ui-meta">No data in the research space yet.</p>
          ) : (
            inputChoices.map((f) => (
              <Checkbox key={f.path} isSelected={inputs.includes(f.path)} onChange={(on) => setInputs((l) => toggle(l, f.path, on))}>
                {f.name}
              </Checkbox>
            ))
          )}
        </fieldset>
        <fieldset className="exp-rinspector__group">
          <legend className="ui-group-label">Outcomes (retained results)</legend>
          {(listing?.results ?? []).length === 0 ? (
            <p className="ui-meta">Keep a figure or table from a run to add it here.</p>
          ) : (
            (listing?.results ?? []).map((r) => (
              <Checkbox key={r.entity} isSelected={outcomes.includes(r.entity)} onChange={(on) => setOutcomes((l) => toggle(l, r.entity, on))}>
                {r.label}
              </Checkbox>
            ))
          )}
        </fieldset>
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
      </div>
      {detail && (
        <InspectorBody>
          <Section title="Attempts" count={detail.attempts.length}>
            {detail.attempts.length === 0 ? (
              <p className="ui-meta">No run of the procedure yet.</p>
            ) : (
              <ul className="exp-research__runs" data-testid="research-attempts">
                {detail.attempts.map((run) => (
                  <RunRow
                    key={run.id}
                    run={run}
                    now={now}
                    title={formatClock(run.startedAtMs)}
                    onSelect={() => selectItem({ kind: "run", runId: run.id })}
                  />
                ))}
              </ul>
            )}
          </Section>
          <GraphDependencies entity={detail.summary.entity} revision={detail.graphRevision} />
        </InspectorBody>
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
    <div className="exp-rinspector" data-testid="research-inspector" data-kind="workflow" data-entity={entity}>
      <InspectorHeader
        title={workflow?.label ?? "Workflow"}
        actions={
          editor && (
            <Button
              size="sm"
              onPress={() => {
                editors.open(resource);
                pages.show(EDITOR_PAGE);
              }}
            >
              Open in the workflow editor
            </Button>
          )
        }
      />
      {!editor && (
        <EmptyState
          fill={false}
          title="The workflow editor is not available"
          description="Saved workflow definitions are listed here read-only."
        />
      )}
      <InspectorBody>
        <GraphDependencies entity={entity} revision={listing?.graphRevision} />
      </InspectorBody>
    </div>
  );
}

function ReferenceInspector({ entity }: { entity: EntityId }) {
  const listing = useSpace((s) => s.listing);
  const reference = listing?.references.find((r) => r.entity === entity);
  if (!reference) return <EmptyState title="Reference not found" />;
  return (
    <div className="exp-rinspector" data-testid="research-inspector" data-kind="reference" data-entity={entity}>
      <InspectorHeader title={reference.label} />
      <div className="exp-rinspector__facts">
        <KeyValueList
          items={[
            { key: "location", label: "Location", value: reference.origin.source, code: true },
            {
              key: "when",
              label: "When referenced",
              value: (
                <>
                  {formatClock(reference.origin.importedAtMs)} · {formatBytes(reference.origin.sizeBytes)} · sha256{" "}
                  <code className="ui-code">{reference.origin.contentHash.slice(0, 12)}…</code>
                </>
              ),
            },
          ]}
        />
        <p className="ui-meta">A read-only reference: Exposition never copies, crawls, or runs it.</p>
      </div>
      <InspectorBody>
        <GraphDependencies entity={entity} revision={listing?.graphRevision} />
      </InspectorBody>
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
    <div className="exp-rinspector" data-testid="research-inspector" data-kind="run" data-run={runId}>
      <InspectorHeader title={`Run ${runId}`} meta={manifest && <span>{manifest.record.outputs.length} outputs</span>} />
      <div className="exp-rinspector__facts">
        <p className="ui-meta">
          Runs are operational history: they stay inspectable here and become part of the graph only through results you keep.
        </p>
      </div>
      <InspectorBody>
        <RunSections runId={runId} />
      </InspectorBody>
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
