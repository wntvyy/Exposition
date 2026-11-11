// Research panel: environment and session controls, code input, streamed run
// output (text, figures, inert tables), run history with provenance, and
// insert/update of figures and tables into documents.
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import {
  FlaskConical,
  FolderOpen,
  Play,
  Power,
  RefreshCw,
  RotateCcw,
  ShieldCheck,
  Square,
  SquareX,
} from "lucide-react";

import type { ArtifactRecord } from "../../bindings/ArtifactRecord";
import type { OutputEntry } from "../../bindings/OutputEntry";
import type { RunManifest } from "../../bindings/RunManifest";
import type { RunSummary } from "../../bindings/RunSummary";
import type { RuntimeEnvironment } from "../../bindings/RuntimeEnvironment";
import { commands, errorMessage } from "../../lib/ipc";
import {
  Banner,
  Button,
  EmptyState,
  IconButton,
  LoadingState,
  Section,
  Select,
  Spinner,
  StatusPill,
  TextField,
  Toolbar,
  UnavailableState,
} from "../../ui/components";
import {
  EMPTY_RUN_OUTPUT,
  attachmentKeys,
  displayArtifacts,
  documentFormat,
  formatBytes,
  formatClock,
  formatDuration,
  isActiveRun,
  runPill,
  sessionPill,
  stripAnsi,
} from "./model";
import {
  artifactText,
  artifactUrl,
  attachArtifact,
  attachTargets,
  cancelRun,
  chooseWorkspaceFolder,
  interruptSession,
  loadEnvironments,
  restartSession,
  runCode,
  selectRun,
  setPanelSelectionReader,
  startResearchTracking,
  startSession,
  stopSession,
  trustWorkspace,
  useResearch,
  userSession,
  type AttachTarget,
} from "./store";
import { parseHtmlTable, type Grid } from "./table";
import "./research.css";

function useNow(active: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [active]);
  return now;
}

export function ResearchPanel() {
  const workspace = useResearch((s) => s.workspace);
  const environments = useResearch((s) => s.environments);
  const environmentsLoading = useResearch((s) => s.environmentsLoading);
  const environmentsError = useResearch((s) => s.environmentsError);
  const error = useResearch((s) => s.error);

  useEffect(() => {
    startResearchTracking();
    if (!useResearch.getState().environments) void loadEnvironments(false);
  }, []);

  if (!workspace) {
    return (
      <div className="exp-research" data-testid="research-panel" role="region" aria-label="Research">
        <EmptyState
          icon={FlaskConical}
          title="No project folder for research"
          description="Run Python, R, or Julia next to your paper. Runs, figures, and tables are recorded in the project's .exposition folder."
          actions={
            <Button variant="primary" icon={FolderOpen} onPress={() => void chooseWorkspaceFolder()}>
              Open folder…
            </Button>
          }
        />
        {error && (
          <Banner tone="danger" title="Research error">
            {error}
          </Banner>
        )}
      </div>
    );
  }

  return (
    <div className="exp-research" data-testid="research-panel" role="region" aria-label="Research">
      <SessionBar />
      {error && (
        <Banner
          tone="danger"
          title="Research error"
          actions={
            <Button size="sm" variant="ghost" onPress={() => useResearch.setState({ error: null })}>
              Dismiss
            </Button>
          }
        >
          <span data-testid="research-error">{error}</span>
        </Banner>
      )}
      {!workspace.trusted && (
        <Banner
          tone="warning"
          title="Workspace not trusted"
          actions={
            <Button size="sm" icon={ShieldCheck} onPress={() => void trustWorkspace()}>
              Trust workspace…
            </Button>
          }
        >
          Research kernels run code with your account&rsquo;s permissions; trust this folder to start a session.
        </Banner>
      )}
      {environmentsLoading && !environments ? (
        <LoadingState title="Looking for Python, R, and Julia…" />
      ) : environmentsError ? (
        <Banner tone="danger" title="Could not discover research environments">
          {environmentsError}
        </Banner>
      ) : (
        <div className="exp-research__main">
          <div className="exp-research__work">
            <EnvironmentNotice />
            <CodeInput />
            <RunOutputView />
          </div>
          <RunHistory />
        </div>
      )}
    </div>
  );
}

function environmentLabel(env: RuntimeEnvironment): string {
  return env.availability.kind === "available" ? env.displayName : `${env.displayName} — unavailable`;
}

function SessionBar() {
  const workspace = useResearch((s) => s.workspace);
  const environments = useResearch((s) => s.environments);
  const selected = useResearch((s) => s.selectedEnvironment);
  const starting = useResearch((s) => s.starting);
  const environmentsLoading = useResearch((s) => s.environmentsLoading);
  const session = useResearch((s) => userSession(s));
  const pill = sessionPill(session);
  const options = (environments?.runtimes ?? []).map((env) => ({ id: env.id, label: environmentLabel(env) }));
  const selectedEnv = environments?.runtimes.find((r) => r.id === selected);
  const canStart = !session && selectedEnv?.availability.kind === "available" && !starting;
  const crashed = session?.state.kind === "crashed";
  const busy = session?.state.kind === "busy";

  return (
    <Toolbar aria-label="Research session" className="exp-research__bar">
      <Select<string>
        aria-label="Research environment"
        className="exp-research__env"
        options={options}
        value={session?.environmentId ?? selected}
        isDisabled={Boolean(session)}
        onChange={(id) => useResearch.setState({ selectedEnvironment: id })}
        placeholder="Choose environment"
      />
      <IconButton
        icon={RefreshCw}
        size="sm"
        aria-label="Refresh environments"
        isDisabled={environmentsLoading}
        onPress={() => void loadEnvironments(true)}
      />
      <span data-testid="research-session-status" data-state={session?.state.kind ?? "none"}>
        <StatusPill
          status={starting ? "running" : pill.status}
          label={starting ? "Starting kernel" : pill.label}
          detail={session?.kernel ? `${session.kernel.language} ${session.kernel.languageVersion}` : undefined}
        />
      </span>
      {!session && (
        <Button
          size="sm"
          variant="primary"
          icon={Play}
          isDisabled={!canStart}
          isPending={starting}
          onPress={() => selected && void startSession(selected)}
        >
          Start session
        </Button>
      )}
      {session && (
        <>
          <Button size="sm" icon={Square} isDisabled={!busy} onPress={() => void interruptSession()}>
            Interrupt
          </Button>
          <Button
            size="sm"
            variant={crashed ? "primary" : "secondary"}
            icon={RotateCcw}
            isDisabled={session.state.kind === "starting" || session.state.kind === "restarting"}
            onPress={() => void restartSession()}
          >
            Restart
          </Button>
          <IconButton icon={Power} size="sm" aria-label="Stop session" onPress={() => void stopSession()} />
        </>
      )}
      <span className="exp-research__root" title={workspace?.root}>
        <code>{workspace?.root}</code>
      </span>
      <IconButton
        icon={FolderOpen}
        size="sm"
        aria-label="Open a different project folder"
        onPress={() => void chooseWorkspaceFolder()}
      />
    </Toolbar>
  );
}

function EnvironmentNotice() {
  const environments = useResearch((s) => s.environments);
  const selected = useResearch((s) => s.selectedEnvironment);
  const session = useResearch((s) => userSession(s));
  const env = environments?.runtimes.find((r) => r.id === selected);
  if (session?.state.kind === "crashed") {
    return (
      <Banner
        tone="danger"
        title="The research session crashed"
        actions={
          <Button size="sm" icon={RotateCcw} onPress={() => void restartSession()}>
            Restart with a fresh kernel
          </Button>
        }
      >
        <span data-testid="research-crash-reason">{session.state.reason}</span>
      </Banner>
    );
  }
  if (!session && env && env.availability.kind === "unavailable") {
    return (
      <div data-testid="research-unavailable" data-environment={env.id}>
        <UnavailableState
          fill={false}
          title={`${env.displayName} is not available`}
          description={
            <>
              <p>{env.availability.reason}</p>
              <p className="exp-research__guidance">
                <code>{env.availability.guidance}</code>
              </p>
            </>
          }
          actions={
            <Button size="sm" icon={RefreshCw} onPress={() => void loadEnvironments(true)}>
              Check again
            </Button>
          }
        />
      </div>
    );
  }
  return null;
}

function CodeInput() {
  const code = useResearch((s) => s.code);
  const session = useResearch((s) => userSession(s));
  const area = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    setPanelSelectionReader(() => {
      const el = area.current;
      if (!el || el.selectionStart === el.selectionEnd) return null;
      return el.value.slice(el.selectionStart, el.selectionEnd);
    });
    return () => setPanelSelectionReader(null);
  }, []);

  const run = () => {
    const el = area.current;
    const selection = el && el.selectionStart !== el.selectionEnd ? el.value.slice(el.selectionStart, el.selectionEnd) : null;
    const text = selection ?? code;
    if (text.trim()) void runCode(text);
  };

  return (
    <div className="exp-research__input">
      <textarea
        ref={area}
        className="exp-research__code"
        data-testid="research-code"
        aria-label="Code to run in the research session"
        spellCheck={false}
        rows={3}
        value={code}
        placeholder={session ? `Code for ${session.displayName} (Ctrl+Enter runs)` : "Code to run (Ctrl+Enter starts a session and runs)"}
        onChange={(e) => useResearch.setState({ code: e.target.value })}
        onKeyDown={(e) => {
          if (e.key === "Enter" && (e.ctrlKey || e.metaKey) && !e.nativeEvent.isComposing) {
            e.preventDefault();
            run();
          }
        }}
      />
      <Button variant="primary" size="sm" icon={Play} onPress={run} isDisabled={!code.trim()}>
        Run
      </Button>
    </div>
  );
}

function RunOutputView() {
  const runId = useResearch((s) => s.selectedRun);
  const run = useResearch((s) => s.runs.find((r) => r.id === s.selectedRun));
  const output = useResearch((s) => (s.selectedRun ? s.outputs[s.selectedRun] : undefined)) ?? EMPTY_RUN_OUTPUT;
  const viewer = useRef<HTMLDivElement>(null);
  const now = useNow(Boolean(run && isActiveRun(run)));

  useLayoutEffect(() => {
    const el = viewer.current;
    if (el && el.scrollHeight - el.scrollTop - el.clientHeight < 80) el.scrollTop = el.scrollHeight;
  }, [output]);

  if (!runId || !run) {
    return (
      <EmptyState
        fill={false}
        title="No run selected"
        description="Run code above; its output, figures, and tables appear here."
      />
    );
  }
  const pill = runPill(run);
  return (
    <div className="exp-research__output" data-testid="research-output" data-run-id={run.id} data-status={run.status.kind}>
      <div className="exp-research__output-head">
        <span data-testid="research-run-status">
          <StatusPill status={pill.status} label={pill.label} detail={formatDuration(run, now)} />
        </span>
        {run.executionCount !== null && <span className="exp-muted">In [{run.executionCount}]</span>}
        <code className="exp-research__preview">{run.codePreview}</code>
        {isActiveRun(run) && (
          <Button size="sm" icon={SquareX} onPress={() => void cancelRun(run.id)}>
            Cancel run
          </Button>
        )}
      </div>
      {run.status.kind === "interrupted" && (
        <Banner tone="canceled" title="Interrupted">
          The kernel stopped this run; output below is partial and the session state is kept.
        </Banner>
      )}
      {run.status.kind === "canceled" && (
        <Banner tone="canceled" title="Canceled">
          This run was canceled; output below is partial.
        </Banner>
      )}
      {output.truncated && (
        <p className="exp-muted" data-testid="research-output-truncated">
          Older output was dropped (output limit).
        </p>
      )}
      <div ref={viewer} className="exp-research__entries" tabIndex={0} aria-label={`Output of run ${run.id}`}>
        {output.entries.map((entry) => (
          <OutputEntryView key={entry.seq} entry={entry} run={run} />
        ))}
        {isActiveRun(run) && !run.queued && <Spinner size="sm" aria-label="Running" />}
      </div>
      <Provenance run={run} />
    </div>
  );
}

function OutputEntryView({ entry, run }: { entry: OutputEntry; run: RunSummary }) {
  const item = entry.item;
  switch (item.kind) {
    case "stream":
      return (
        <pre className="exp-research__stream" data-stream={item.name} data-testid="research-stream">
          {stripAnsi(item.text)}
        </pre>
      );
    case "display": {
      const { figure, table, html } = displayArtifacts(run, item.artifacts);
      if (figure) return <FigureView run={run} figure={figure} />;
      if (table && html) return <TableView run={run} table={table} html={html} />;
      return (
        <pre className="exp-research__result" data-testid="research-result">
          {item.text ?? ""}
        </pre>
      );
    }
    case "error":
      return (
        <div className="exp-research__error" role="group" aria-label="Error" data-testid="research-error-output">
          <p className="exp-research__error-title">
            {item.ename}: {item.evalue}
          </p>
          {item.traceback.length > 0 && <pre>{item.traceback.map(stripAnsi).join("\n")}</pre>}
        </div>
      );
    case "notice":
      return <p className="exp-research__notice exp-muted">{item.message}</p>;
    case "clearOutput":
      return null;
  }
}

function FigureView({ run, figure }: { run: RunSummary; figure: ArtifactRecord }) {
  const [url, setUrl] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    artifactUrl(run.id, figure).then(
      (u) => live && setUrl(u),
      (e: unknown) => live && setError(errorMessage(e)),
    );
    return () => {
      live = false;
    };
  }, [run.id, figure]);
  return (
    <figure className="exp-research__figure" data-testid="research-figure" data-artifact-id={figure.id}>
      {url ? (
        <img src={url} alt={`Figure from run ${run.id}`} data-testid="research-figure-image" />
      ) : error ? (
        <Banner tone="danger" title="Could not show the figure">
          {error}
        </Banner>
      ) : (
        <Spinner size="sm" aria-label="Loading figure" />
      )}
      <figcaption className="exp-muted">
        {figure.path} · {figure.mediaType} · {formatBytes(figure.sizeBytes)}
      </figcaption>
      <AttachControls run={run} artifact={figure} />
    </figure>
  );
}

function TableView({ run, table, html }: { run: RunSummary; table: ArtifactRecord; html: ArtifactRecord }) {
  const [grid, setGrid] = useState<Grid | null | undefined>(undefined);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    artifactText(run.id, html).then(
      (text) => live && setGrid(parseHtmlTable(text)),
      (e: unknown) => live && setError(errorMessage(e)),
    );
    return () => {
      live = false;
    };
  }, [run.id, html]);
  return (
    <div className="exp-research__table" data-testid="research-table" data-artifact-id={table.id}>
      {error ? (
        <Banner tone="danger" title="Could not show the table">
          {error}
        </Banner>
      ) : grid === undefined ? (
        <Spinner size="sm" aria-label="Loading table" />
      ) : grid === null ? (
        <p className="exp-muted">The output contained no table.</p>
      ) : (
        <div className="exp-research__grid-scroll" tabIndex={0} aria-label="Table output">
          <table className="exp-research__grid">
            {grid.head.length > 0 && (
              <thead>
                {grid.head.map((row, r) => (
                  <tr key={r}>
                    {row.map((cell, c) => (
                      <th key={c} colSpan={cell.colSpan} rowSpan={cell.rowSpan} scope="col">
                        {cell.text}
                      </th>
                    ))}
                  </tr>
                ))}
              </thead>
            )}
            <tbody>
              {grid.body.map((row, r) => (
                <tr key={r}>
                  {row.map((cell, c) =>
                    cell.header ? (
                      <th key={c} colSpan={cell.colSpan} rowSpan={cell.rowSpan} scope="row">
                        {cell.text}
                      </th>
                    ) : (
                      <td key={c} colSpan={cell.colSpan} rowSpan={cell.rowSpan}>
                        {cell.text}
                      </td>
                    ),
                  )}
                </tr>
              ))}
            </tbody>
          </table>
          {grid.truncated && <p className="exp-muted">Showing 200 of {grid.totalRows} rows.</p>}
        </div>
      )}
      <AttachControls run={run} artifact={table} />
    </div>
  );
}

function AttachControls({ run, artifact }: { run: RunSummary; artifact: ArtifactRecord }) {
  const [open, setOpen] = useState(false);
  const [targets, setTargets] = useState<AttachTarget[] | null>(null);
  const [target, setTarget] = useState<string | null>(null);
  const [keys, setKeys] = useState<string[]>([]);
  const [key, setKey] = useState<string | null>(null);
  const [caption, setCaption] = useState("");
  const [busy, setBusy] = useState(false);
  const what = artifact.kind === "table" ? "table" : "figure";

  useEffect(() => {
    if (!open) return;
    let live = true;
    attachTargets().then(
      (list) => {
        if (!live) return;
        setTargets(list);
        setTarget((current) => current ?? list[0]?.document ?? null);
      },
      () => live && setTargets([]),
    );
    return () => {
      live = false;
    };
  }, [open]);

  const selected = targets?.find((t) => t.document === target) ?? null;
  useEffect(() => {
    if (!selected) return;
    let live = true;
    const format = documentFormat(selected.path);
    commands.documentState(selected.document).then(
      (state) => {
        if (!live || !format) return;
        const found = attachmentKeys(state.text, format);
        setKeys(found);
        setKey((current) => (current && found.includes(current) ? current : (found[0] ?? null)));
      },
      () => live && setKeys([]),
    );
    return () => {
      live = false;
    };
  }, [selected, busy]);

  if (!open) {
    return (
      <div className="exp-research__attach">
        <Button size="sm" variant="ghost" onPress={() => setOpen(true)}>
          Insert or update in document…
        </Button>
      </div>
    );
  }
  const act = async (mode: { kind: "insert" } | { kind: "update"; key: string }) => {
    if (!selected) return;
    setBusy(true);
    await attachArtifact({ run, artifact, target: selected, mode, caption: caption || null });
    setBusy(false);
  };
  return (
    <div className="exp-research__attach" data-testid="research-attach" role="group" aria-label={`Attach this ${what}`}>
      {targets === null ? (
        <Spinner size="sm" aria-label="Loading documents" />
      ) : targets.length === 0 ? (
        <p className="exp-muted">Open a Typst, LaTeX, or Markdown document to insert this {what}.</p>
      ) : (
        <>
          <Select<string>
            label="Document"
            options={targets.map((t) => ({ id: t.document, label: t.path }))}
            value={target}
            onChange={setTarget}
          />
          <TextField
            label="Caption"
            value={caption}
            onChange={setCaption}
            placeholder={keys.length ? "Leave empty to keep the existing caption on update" : `Caption for the ${what}`}
          />
          <div className="exp-research__attach-actions">
            <Button size="sm" variant="primary" isPending={busy} onPress={() => void act({ kind: "insert" })}>
              Insert into document
            </Button>
            {keys.length > 0 && (
              <>
                <Select<string>
                  aria-label="Attachment to update"
                  options={keys.map((k) => ({ id: k, label: k }))}
                  value={key}
                  onChange={setKey}
                />
                <Button size="sm" isPending={busy} isDisabled={!key} onPress={() => key && void act({ kind: "update", key })}>
                  Update in document
                </Button>
              </>
            )}
          </div>
        </>
      )}
    </div>
  );
}

function Provenance({ run }: { run: RunSummary }) {
  const [manifest, setManifest] = useState<RunManifest | null>(null);
  const finished = !isActiveRun(run);
  useEffect(() => {
    if (!finished) return;
    let live = true;
    commands.researchRun(run.id).then(
      (m) => live && setManifest(m),
      () => live && setManifest(null),
    );
    return () => {
      live = false;
    };
  }, [run.id, finished]);
  if (!manifest) return null;
  const origin = manifest.code.origin;
  return (
    <Section title="Provenance" collapsible defaultExpanded={false} className="exp-research__provenance">
      <dl className="exp-research__facts" data-testid="research-provenance">
        <dt>Manifest</dt>
        <dd>
          <code>.exposition/runs/{manifest.record.id}.json</code>
        </dd>
        <dt>Code</dt>
        <dd>
          {origin.kind === "input"
            ? "Research input"
            : origin.kind === "file"
              ? `File ${origin.path}${origin.revision !== null ? ` (revision ${origin.revision})` : ""}`
              : `Selection of ${origin.path ?? "an unsaved document"} (revision ${origin.revision})`}
          {" · "}
          <code title={manifest.code.sha256}>sha256 {manifest.code.sha256.slice(0, 16)}…</code>
        </dd>
        {manifest.record.inputs.map((input) => (
          <InputFact key={input.path} path={input.path} revision={input.revision} hash={input.contentHash} />
        ))}
        <dt>Kernel</dt>
        <dd>
          {manifest.session.kernel.implementation} {manifest.session.kernel.implementationVersion} ·{" "}
          {manifest.session.kernel.language} {manifest.session.kernel.languageVersion} · session generation{" "}
          {manifest.session.generation}
        </dd>
        <dt>Interpreter</dt>
        <dd>
          {manifest.session.interpreter.implementation} {manifest.session.interpreter.version} ·{" "}
          <code>{manifest.session.interpreter.location}</code>
        </dd>
        <dt>Versions</dt>
        <dd>{manifest.record.environment.map((t) => `${t.name} ${t.version}`).join(" · ")}</dd>
        <dt>Started</dt>
        <dd>
          {formatClock(manifest.record.startedAtMs)}
          {manifest.record.finishedAtMs !== null && ` · finished ${formatClock(manifest.record.finishedAtMs)}`}
        </dd>
        {manifest.record.outputs.map((a) => (
          <ArtifactFact key={a.id} artifact={a} />
        ))}
      </dl>
    </Section>
  );
}

function InputFact({ path, revision, hash }: { path: string; revision: number | null; hash: string | null }) {
  return (
    <>
      <dt>Input</dt>
      <dd>
        <code>{path}</code>
        {revision !== null && ` · revision ${revision}`}
        {hash && (
          <>
            {" · "}
            <code title={hash}>{hash.slice(0, 23)}…</code>
          </>
        )}
      </dd>
    </>
  );
}

function ArtifactFact({ artifact }: { artifact: ArtifactRecord }) {
  return (
    <>
      <dt>{artifact.kind === "figure" ? "Figure" : artifact.kind === "table" ? "Table" : "Output"}</dt>
      <dd>
        <code>{artifact.path}</code> · {formatBytes(artifact.sizeBytes)} ·{" "}
        <code title={artifact.contentHash}>{artifact.contentHash.slice(0, 23)}…</code>
      </dd>
    </>
  );
}

function RunHistory() {
  const runs = useResearch((s) => s.runs);
  const selected = useResearch((s) => s.selectedRun);
  const visible = useMemo(() => runs.slice(0, 200), [runs]);
  const now = useNow(runs.some(isActiveRun));
  return (
    <div className="exp-research__history">
      <h3 className="exp-research__history-title">Runs</h3>
      {visible.length === 0 ? (
        <EmptyState fill={false} title="No runs yet" description="Every run is recorded with its inputs and versions." />
      ) : (
        <ul className="exp-research__runs" aria-label="Run history" data-testid="research-history">
          {visible.map((run) => {
            const pill = runPill(run);
            return (
              <li key={run.id} className="exp-research__run" data-selected={run.id === selected || undefined}>
                <button
                  type="button"
                  className="exp-research__run-select"
                  aria-current={run.id === selected || undefined}
                  data-run-id={run.id}
                  onClick={() => void selectRun(run.id)}
                >
                  <span className="exp-research__run-code">{run.codePreview || "(empty)"}</span>
                  <span className="exp-research__run-meta">
                    <StatusPill size="sm" status={pill.status} label={pill.label} detail={formatDuration(run, now)} />
                    <span className="exp-muted">
                      {formatClock(run.startedAtMs)}
                      {run.owner.kind === "agent" && " · agent"}
                      {run.outputs.some((a) => a.kind === "figure") && " · figure"}
                      {run.outputs.some((a) => a.kind === "table") && " · table"}
                    </span>
                  </span>
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
