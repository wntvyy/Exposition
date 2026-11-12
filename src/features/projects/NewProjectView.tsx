// New Project page (editor tab `new-project:`): start from a preset — choose
// options, preview exactly what will be created, choose a location, create —
// or describe an objective and let an agent organize a new empty project.
// Creating writes only the previewed scaffold: no trust, no code execution,
// no installs, nothing sent to a model (the objective path's agent task
// follows the provider settings and the permission chosen here).
import { FolderOpen, FolderPlus, Sparkles } from "lucide-react";
import { useEffect, useId } from "react";

import type { BlueprintDiagnostic } from "../../bindings/BlueprintDiagnostic";
import type { CapabilityStatus } from "../../bindings/CapabilityStatus";
import type { EditorProps } from "../../app/registry";
import {
  Banner,
  Button,
  Checkbox,
  ErrorState,
  LoadingState,
  SegmentedControl,
  Select,
  Spinner,
  StatusPill,
  TextField,
} from "../../ui/components";
import { EXECUTION_LABELS, PERMISSION_LABELS } from "../agents/format";
import { declaredCapabilities, FORMAT_LABELS, RUNTIME_LABELS } from "./model";
import {
  browseLocation,
  createFromObjective,
  effectiveLeanToolchain,
  createFromPreset,
  loadCatalog,
  setLocation,
  setOptions,
  useProjects,
  type NewProjectMode,
} from "./store";

function TextArea(props: { label: string; value: string; onChange: (v: string) => void; rows?: number; testId?: string; placeholder?: string }) {
  const id = useId();
  return (
    <div className="ui-field">
      <label className="ui-field__label" htmlFor={id}>
        {props.label}
      </label>
      <textarea
        id={id}
        className="ui-input xp-proj__textarea"
        rows={props.rows ?? 4}
        value={props.value}
        placeholder={props.placeholder}
        onChange={(e) => props.onChange(e.target.value)}
        data-testid={props.testId}
      />
    </div>
  );
}

function CapabilityRow({ status }: { status: CapabilityStatus }) {
  return (
    <li className="xp-proj__capability" data-capability={status.capability} data-available={status.available}>
      <span className="xp-proj__capability-name">{status.title}</span>
      <StatusPill status={status.available ? "succeeded" : "unavailable"} label={status.available ? "Available" : "Not available"} />
      <span className="xp-proj__muted">{status.detail}</span>
      {!status.available && status.guidance && <span className="xp-proj__guidance">{status.guidance}</span>}
    </li>
  );
}

function Diagnostics({ items }: { items: BlueprintDiagnostic[] }) {
  if (items.length === 0) return null;
  const errors = items.filter((d) => d.severity === "error");
  return (
    <Banner tone={errors.length ? "danger" : "warning"} title={errors.length ? "This blueprint cannot be created" : "Notes"}>
      <ul className="xp-proj__list" data-testid="project-diagnostics">
        {items.map((d, i) => (
          <li key={i} data-code={d.code}>
            {d.message}
          </li>
        ))}
      </ul>
    </Banner>
  );
}

function Location() {
  const location = useProjects((s) => s.location);
  const destination = useProjects((s) => s.destination);
  return (
    <section className="xp-proj__group" aria-label="Location">
      <h3 className="xp-proj__group-title">Location</h3>
      <div className="xp-proj__row">
        <TextField
          className="xp-proj__grow"
          label="Project folder"
          description="A new folder (its parent must exist) or an existing empty folder. Existing files are never overwritten."
          code
          value={location}
          onChange={setLocation}
          data-testid="project-location"
        />
        <Button icon={FolderOpen} onPress={() => void browseLocation()}>
          Browse…
        </Button>
      </div>
      {destination && (
        <p
          className={destination.ok ? "xp-proj__muted" : "xp-proj__error"}
          data-testid="project-destination-status"
          data-ok={destination.ok}
          role={destination.ok ? undefined : "alert"}
        >
          {destination.ok
            ? destination.createsFolder
              ? `A new folder will be created: ${destination.path}`
              : `The empty folder ${destination.path} will be used.`
            : destination.message}
        </p>
      )}
    </section>
  );
}

function Preview() {
  const preview = useProjects((s) => s.preview);
  const previewError = useProjects((s) => s.previewError);
  const blueprint = useProjects((s) => s.blueprint);
  const report = useProjects((s) => s.report);
  const reportError = useProjects((s) => s.reportError);
  if (previewError) return <ErrorState title="Could not prepare this preset" description={previewError} />;
  if (!preview) return <LoadingState title="Preparing the preview…" />;
  const s = preview.summary;
  const capabilities = blueprint ? declaredCapabilities(blueprint, report) : [];
  const declares = (blueprint?.capabilities.length ?? 0) > 0;
  return (
    <section className="xp-proj__group" aria-label="What will be created" data-testid="project-preview" data-valid={preview.valid}>
      <h3 className="xp-proj__group-title">What will be created</h3>
      <Diagnostics items={preview.diagnostics} />
      {s && (
        <div className="xp-proj__preview">
          <div>
            <h4 className="xp-proj__subtitle">Files</h4>
            <ul className="xp-proj__files" data-testid="project-preview-files">
              {s.files.map((f) => (
                <li key={f.path}>
                  <code>{f.path}</code>
                  <span className="xp-proj__muted">
                    {f.bytes} B{f.roles.length ? ` · ${f.roles.join(", ")}` : " · not a project resource"}
                  </span>
                </li>
              ))}
              {s.folders.map((f) => (
                <li key={`d:${f}`}>
                  <code>{f}/</code>
                  <span className="xp-proj__muted">folder</span>
                </li>
              ))}
              {s.files.length === 0 && s.folders.length === 0 && <li className="xp-proj__muted">No files: an empty project.</li>}
            </ul>
          </div>
          <div>
            <h4 className="xp-proj__subtitle">Project structure</h4>
            <ul className="xp-proj__list" data-testid="project-preview-entities">
              {s.research && <li>Research space “{s.research}”</li>}
              {s.entities.map((e, i) => (
                <li key={i}>
                  {e.label} <span className="xp-proj__muted">({e.kind}{e.source ? ` in ${e.source}` : ""})</span>
                </li>
              ))}
              {s.collections.map((c) => (
                <li key={c}>Collection {c}</li>
              ))}
              {s.relations.map((r) => (
                <li key={r} className="xp-proj__muted">
                  {r}
                </li>
              ))}
              {s.workflows.map((w) => (
                <li key={w.title}>
                  Workflow draft “{w.title}” — {w.wellFormed ? "well formed" : `incomplete (${w.errors})`}, not runnable in this version
                </li>
              ))}
              {!s.research && s.entities.length === 0 && s.workflows.length === 0 && (
                <li className="xp-proj__muted">Only the project itself.</li>
              )}
            </ul>
            <p className="xp-proj__muted">
              {s.hasManuscript ? "Opens the manuscript in the Editor." : s.entry.kind === "research" ? "Research-only: opens the Research page." : "Opens the project's files."}
            </p>
          </div>
        </div>
      )}
      {declares && (
        <>
          <h4 className="xp-proj__subtitle">Required tools (declared, never installed or run)</h4>
          {capabilities ? (
            <ul className="xp-proj__capabilities" data-testid="project-capabilities">
              {capabilities.map((c) => (
                <CapabilityRow key={c.capability} status={c} />
              ))}
            </ul>
          ) : reportError ? (
            <p className="xp-proj__error">Could not check installed tools: {reportError}</p>
          ) : (
            <p className="xp-proj__muted xp-proj__checking" data-testid="project-capabilities-checking">
              <Spinner size="sm" aria-label="Checking" /> Checking which tools are installed…
            </p>
          )}
        </>
      )}
    </section>
  );
}

function PresetForm() {
  const catalog = useProjects((s) => s.catalog);
  const report = useProjects((s) => s.report);
  const presetId = useProjects((s) => s.presetId);
  const title = useProjects((s) => s.title);
  const format = useProjects((s) => s.format);
  const runtime = useProjects((s) => s.runtime);
  const leanToolchain = useProjects((s) => effectiveLeanToolchain(s));
  const includeWorkflow = useProjects((s) => s.includeWorkflow);
  if (!catalog) return null;
  const preset = catalog.presets.find((p) => p.id === presetId) ?? catalog.presets[0];
  if (!preset) return null;
  const installedLean = report?.leanToolchains ?? [];
  const lean = [...installedLean];
  if (!lean.includes(catalog.defaultLeanToolchain)) lean.push(catalog.defaultLeanToolchain);
  const runtimes = report?.runtimes ?? (["python", "r", "julia"] as const).map((runtime) => ({ runtime, available: false, detail: "Checking…" }));
  return (
    <section className="xp-proj__group" aria-label="Starting structure">
      <h3 className="xp-proj__group-title">Starting structure</h3>
      <div className="xp-proj__form">
        <Select
          label="Preset"
          value={preset.id}
          onChange={(id) => setOptions({ presetId: String(id) })}
          options={catalog.presets.map((p) => ({ id: p.id, label: p.title, description: p.description }))}
          data-testid="preset-select"
        />
        <p className="xp-proj__muted xp-proj__wide">{preset.description}</p>
        <TextField label="Title" value={title} placeholder={preset.title} onChange={(v) => setOptions({ title: v })} data-testid="project-title" />
        {preset.manuscript && (
          <Select
            label="Manuscript format"
            value={format}
            onChange={(v) => setOptions({ format: v })}
            options={(Object.keys(FORMAT_LABELS) as (keyof typeof FORMAT_LABELS)[]).map((id) => ({ id, label: FORMAT_LABELS[id] }))}
          />
        )}
        {preset.computation && (
          <Select
            label="Research runtime"
            value={runtime}
            onChange={(v) => setOptions({ runtime: v })}
            options={runtimes.map((r) => ({
              id: r.runtime,
              label: `${RUNTIME_LABELS[r.runtime]} — ${report ? (r.available ? "installed" : "not found") : "checking…"}`,
              description: r.detail,
            }))}
          />
        )}
        {preset.lean && (
          <Select
            label="Lean toolchain"
            value={leanToolchain ?? catalog.defaultLeanToolchain}
            onChange={(v) => setOptions({ leanToolchain: String(v) })}
            options={lean.map((t) => ({
              id: t,
              label: installedLean.includes(t) ? t : report ? `${t} (not installed)` : t,
            }))}
          />
        )}
        {preset.workflow && (
          <Checkbox isSelected={includeWorkflow} onChange={(v) => setOptions({ includeWorkflow: v })}>
            Include a {preset.workflow === "formalAudit" ? "Formal audit" : "Numerical experiment"} workflow draft (saved, not runnable in this version)
          </Checkbox>
        )}
      </div>
    </section>
  );
}

function ObjectiveForm() {
  const objective = useProjects((s) => s.objective);
  const title = useProjects((s) => s.title);
  const permission = useProjects((s) => s.permission);
  const execution = useProjects((s) => s.execution);
  return (
    <section className="xp-proj__group" aria-label="Objective">
      <h3 className="xp-proj__group-title">Objective</h3>
      <div className="xp-proj__form">
        <div className="xp-proj__wide">
          <TextArea
            label="What is this project for?"
            value={objective}
            onChange={(v) => setOptions({ objective: v })}
            rows={5}
            testId="objective-text"
            placeholder="e.g. Investigate whether the spectral gap of random regular graphs concentrates, with numerical experiments."
          />
        </div>
        <TextField label="Title" value={title} onChange={(v) => setOptions({ title: v })} data-testid="project-title" />
        <Select label="Document changes" value={permission} onChange={(v) => setOptions({ permission: v })} options={(Object.keys(PERMISSION_LABELS) as (keyof typeof PERMISSION_LABELS)[]).map((id) => ({ id, label: PERMISSION_LABELS[id] }))} />
        <Select label="Commands and code" value={execution} onChange={(v) => setOptions({ execution: v })} options={(Object.keys(EXECUTION_LABELS) as (keyof typeof EXECUTION_LABELS)[]).map((id) => ({ id, label: EXECUTION_LABELS[id] }))} />
      </div>
      <Banner tone="info" title="How this works">
        Exposition creates an empty project in the folder below, opens it, and starts an agent task there with the whole project in scope. The agent proposes an organization — it may be research-only — and creates files, research structure, and project entities through the same checked tools people use. It works only inside the new project, and its changes follow the permission you choose. Content is sent to your model provider according to your provider settings.
      </Banner>
    </section>
  );
}

export function NewProjectView(_props: EditorProps) {
  const mode = useProjects((s) => s.mode);
  const catalog = useProjects((s) => s.catalog);
  const catalogError = useProjects((s) => s.catalogError);
  const creating = useProjects((s) => s.creating);
  const error = useProjects((s) => s.error);
  const preview = useProjects((s) => s.preview);
  const destination = useProjects((s) => s.destination);
  const objective = useProjects((s) => s.objective);

  useEffect(() => {
    void loadCatalog();
  }, []);

  if (catalogError) {
    return (
      <div className="xp-proj">
        <ErrorState title="Could not load the project presets" description={catalogError} actions={<Button onPress={() => void loadCatalog()}>Retry</Button>} />
      </div>
    );
  }
  if (!catalog) return <LoadingState title="Loading presets…" />;
  const ready = Boolean(destination?.ok);
  const canCreate = mode === "preset" ? ready && Boolean(preview?.valid) : ready && objective.trim().length > 0;
  return (
    <div className="xp-proj" data-testid="new-project" data-mode={mode}>
      <div className="xp-proj__page">
        <header className="xp-proj__header">
          <h2 className="xp-proj__title">New project</h2>
          <p className="xp-proj__muted">Start from a preset, or describe an objective and let an agent organize the project. A project can be research-only — a manuscript is optional.</p>
          <SegmentedControl<NewProjectMode>
            aria-label="How to start"
            value={mode}
            onChange={(next) => setOptions({ mode: next })}
            options={[
              { id: "preset", label: "Start from a preset" },
              { id: "objective", label: "Describe an objective" },
            ]}
            data-testid="new-project-mode"
          />
        </header>
        {mode === "preset" ? <PresetForm /> : <ObjectiveForm />}
        <Location />
        {mode === "preset" && <Preview />}
        {error && (
          <Banner tone="danger" title="The project was not created" live>
            {error}
          </Banner>
        )}
        <p className="xp-proj__muted" data-testid="project-authorization">
          Creating a project writes only its starting files and records its structure. It does not trust the folder, run any code, install anything, or send anything to a model provider.
        </p>
        <footer className="xp-proj__footer">
          {mode === "preset" ? (
            <Button variant="primary" icon={FolderPlus} isPending={creating} isDisabled={!canCreate || creating} onPress={() => void createFromPreset()} data-testid="project-create">
              Create Project
            </Button>
          ) : (
            <Button variant="primary" icon={Sparkles} isPending={creating} isDisabled={!canCreate || creating} onPress={() => void createFromObjective()} data-testid="objective-create">
              Create and Start Agent
            </Button>
          )}
        </footer>
      </div>
    </div>
  );
}
