// Research panel: environment and session controls, code input, streamed run
// output (text, figures, inert tables), run history with provenance, and
// insert/update of figures and tables into documents.
import { formatBytes, formatClock } from "../../lib/format";
import { memo, useEffect, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent, type MouseEvent } from "react";
import {
  FlaskConical,
  FolderOpen,
  OctagonPause,
  Play,
  Power,
  RefreshCcwDot,
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
  KeyValueList,
  ListRow,
  LoadingState,
  PanelHeader,
  Section,
  Select,
  Spinner,
  StatusPill,
  StatusText,
  Toolbar,
  type KeyValueItem,
} from "../../ui/components";
import { runnerAvailabilityChanged } from "../workflows/availability";
import {
  EMPTY_RUN_OUTPUT,
  ENTRY_PREVIEW_CHARS,
  displayArtifacts,
  formatDuration,
  isActiveRun,
  runPill,
  sessionPill,
  stripAnsi,
} from "./model";
import { useOpList } from "../operations/useOpList";
import { SetupArea, SetupButton } from "./EnvironmentSetup";
import { ExecutionLabel } from "./ExecutionLabel";
import { PromotionControls, RetainControls } from "./Promotion";
import { researchRunActions } from "./researchOps";
import { activeUserRunId, authorizationText, lastUserRun, rerunDescription } from "./runloop";
import { startSetupTracking } from "./setup";
import { startSpaceTracking } from "./space";
import {
  artifactText,
  artifactUrl,
  cancelRun,
  chooseEnvironment,
  interruptSession,
  loadEnvironments,
  openProjectFolder,
  rerun,
  restartSession,
  runCode,
  findRun,
  loadMoreRuns,
  selectRun,
  setPanelSelectionReader,
  startResearchTracking,
  startSession,
  stopRun,
  stopSession,
  trustWorkspace,
  useResearch,
  userSession,
} from "./store";
import { parseHtmlTable, type Grid } from "./table";
import "./research.css";

export function useNow(active: boolean): number {
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
    startSpaceTracking();
    startSetupTracking();
    if (!useResearch.getState().environments) void loadEnvironments(false);
  }, []);

  if (!workspace) {
    return (
      <div className="exp-research" data-testid="research-panel" role="region" aria-label="Research">
        <EmptyState
          icon={FlaskConical}
          title="No folder open"
          description="Open a project folder to run Python, R, or Julia. Runs and their outputs are recorded in its .exposition folder."
          actions={
            <Button size="sm" icon={FolderOpen} onPress={() => void openProjectFolder()}>
              Open Folder…
            </Button>
          }
        />
        <div className="exp-research__banners">
          {error && (
            <Banner tone="danger" title="Research error">
              {error}
            </Banner>
          )}
        </div>
      </div>
    );
  }

  return (
    <div className="exp-research" data-testid="research-panel" role="region" aria-label="Research">
      <SessionBar />
      <div className="exp-research__banners">
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
            Research kernels run code with your account&rsquo;s privileges; trust this folder to start a session.
          </Banner>
        )}
        {!(environmentsLoading && !environments) && environmentsError && (
          <Banner tone="danger" title="Could not discover research environments">
            {environmentsError}
          </Banner>
        )}
      </div>
      {environmentsLoading && !environments ? (
        <LoadingState title="Looking for Python, R, and Julia…" />
      ) : environmentsError ? null : (
        <div className="exp-research__main">
          <div className="exp-research__work ui-inset-reset">
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

/** The research session's headline state (one per surface: the page header or the panel's session bar). */
export function SessionStatus() {
  const starting = useResearch((s) => s.starting);
  const session = useResearch((s) => userSession(s));
  const pill = sessionPill(session);
  return (
    <span data-testid="research-session-status" data-state={session?.state.kind ?? "none"}>
      <StatusPill
        status={starting ? "running" : pill.status}
        label={starting ? "Starting kernel" : pill.label}
        detail={session?.kernel ? `${session.kernel.language} ${session.kernel.languageVersion}` : undefined}
      />
    </span>
  );
}

/**
 * Environment and session controls, the execution-policy label, and the
 * project folder. The Research page shows the session state in its header
 * (`status={false}`); the panel shows it here.
 */
export function SessionBar({ status = true }: { status?: boolean }) {
  const workspace = useResearch((s) => s.workspace);
  const environments = useResearch((s) => s.environments);
  const selected = useResearch((s) => s.selectedEnvironment);
  const starting = useResearch((s) => s.starting);
  const environmentsLoading = useResearch((s) => s.environmentsLoading);
  const session = useResearch((s) => userSession(s));
  const options = (environments?.runtimes ?? []).map((env) => ({ id: env.id, label: environmentLabel(env) }));
  const selectedEnv = environments?.runtimes.find((r) => r.id === selected);
  const detached = session?.state.kind === "detached";
  const live = session && !detached ? session : undefined;
  const canStart = !live && selectedEnv?.availability.kind === "available" && !starting;
  const crashed = session?.state.kind === "crashed";
  const busy = session?.state.kind === "busy";

  return (
    <Toolbar aria-label="Research session" className="exp-research__bar">
      <Select<string>
        aria-label="Research environment"
        className="exp-research__env"
        options={options}
        value={live?.environmentId ?? selected}
        isDisabled={Boolean(live)}
        onChange={(id) => chooseEnvironment(id)}
        placeholder="Choose environment"
      />
      {/* Interactive environments, then restricted batch availability (Docker, images). */}
      <IconButton
        icon={RefreshCw}
        size="sm"
        aria-label="Refresh environments"
        isDisabled={environmentsLoading}
        onPress={() => void loadEnvironments(true).finally(runnerAvailabilityChanged)}
      />
      <SetupButton />
      {status && <SessionStatus />}
      {!live && (
        <Button
          size="sm"
          variant="ghost"
          icon={Play}
          isDisabled={!canStart}
          isPending={starting}
          onPress={() => selected && void startSession(selected)}
        >
          Start session
        </Button>
      )}
      {live && (
        <>
          <Button size="sm" variant="ghost" icon={OctagonPause} isDisabled={!busy} onPress={() => void interruptSession()}>
            Interrupt
          </Button>
          <Button
            size="sm"
            variant={crashed ? "primary" : "ghost"}
            icon={RefreshCcwDot}
            isDisabled={live.state.kind === "starting" || live.state.kind === "restarting"}
            onPress={() => void restartSession()}
          >
            Restart
          </Button>
          <IconButton icon={Power} size="sm" aria-label="Stop session" onPress={() => void stopSession()} />
        </>
      )}
      <ExecutionLabel />
      <span className="exp-research__root ui-meta" title={workspace?.root}>
        <code className="ui-code">{workspace?.root}</code>
      </span>
    </Toolbar>
  );
}

/** Session problems and environment setup above the run bar (page and panel). */
export function EnvironmentNotice() {
  return (
    <>
      <SessionNotice />
      <SetupArea />
    </>
  );
}

function SessionNotice() {
  const session = useResearch((s) => userSession(s));
  if (session?.state.kind === "detached") {
    return (
      <Banner
        tone="stale"
        title="This session was stopped"
        actions={
          <Button size="sm" icon={Play} onPress={() => void startSession(session.environmentId)}>
            Start a new session
          </Button>
        }
      >
        <span data-testid="research-detached-reason">{session.state.reason}</span>
      </Banner>
    );
  }
  if (session?.state.kind === "crashed") {
    return (
      <Banner
        tone="danger"
        title="The research session crashed"
        actions={
          <Button size="sm" icon={RefreshCcwDot} onPress={() => void restartSession()}>
            Restart with a fresh kernel
          </Button>
        }
      >
        <span data-testid="research-crash-reason">{session.state.reason}</span>
      </Banner>
    );
  }
  return null;
}

/**
 * The run loop with the same controls everywhere: Run (Play), Stop (Square:
 * cancel the running code), Rerun (RotateCcw: the last run's kind of input
 * against the current source). Direct user actions: no extra approval.
 */
export function RunControls({
  onRun,
  runDisabled,
  runLabel = "Run",
  runAccessibleName,
}: {
  onRun: () => void;
  runDisabled?: boolean;
  runLabel?: string;
  runAccessibleName?: string;
}) {
  const active = useResearch((s) => activeUserRunId(userSession(s), s.runs));
  const last = useResearch((s) => lastUserRun(s.runs));
  const starting = useResearch((s) => s.starting);
  // While the selected environment needs setup, its notice holds the one primary action.
  const needsSetup = useResearch((s) => {
    const env = s.environments?.runtimes.find((r) => r.id === s.selectedEnvironment);
    return !userSession(s) && env?.availability.kind === "unavailable";
  });
  const rerunName = rerunDescription(last);
  return (
    <div className="exp-research__run-controls" role="group" aria-label="Run controls" data-testid="research-run-controls">
      <Button
        variant={needsSetup ? "secondary" : "primary"}
        size="sm"
        icon={Play}
        onPress={onRun}
        isDisabled={runDisabled}
        aria-label={runAccessibleName}
        data-testid="research-run"
      >
        {runLabel}
      </Button>
      <Button
        size="sm"
        variant="ghost"
        icon={Square}
        isDisabled={!active}
        onPress={() => void stopRun()}
        aria-label="Stop the running code"
        data-testid="research-stop"
      >
        Stop
      </Button>
      <Button
        size="sm"
        variant="ghost"
        icon={RotateCcw}
        isDisabled={!last || starting}
        onPress={() => void rerun()}
        aria-label={rerunName}
        data-testid="research-rerun"
        data-origin={last?.origin.kind}
      >
        Rerun
      </Button>
    </div>
  );
}

export function CodeInput() {
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
      <RunControls onRun={run} runDisabled={!code.trim()} runAccessibleName="Run the Research input" />
    </div>
  );
}

export function RunOutputView() {
  const runId = useResearch((s) => s.selectedRun);
  const run = useResearch((s) => findRun(s, s.selectedRun));
  const output = useResearch((s) => (s.selectedRun ? s.outputs[s.selectedRun] : undefined)) ?? EMPTY_RUN_OUTPUT;
  const viewer = useRef<HTMLDivElement>(null);
  const now = useNow(Boolean(run && isActiveRun(run)));
  // The user's executing run is stopped with Stop in the run controls; Cancel
  // run is for a queued run or another session's run.
  const stoppable = useResearch((s) => activeUserRunId(userSession(s), s.runs));

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
        {run.executionCount !== null && <span className="ui-meta">In [{run.executionCount}]</span>}
        <code className="exp-research__preview">{run.codePreview}</code>
        {isActiveRun(run) && run.id !== stoppable && (
          <Button size="sm" variant="ghost" icon={SquareX} onPress={() => void cancelRun(run.id)}>
            Cancel run
          </Button>
        )}
      </div>
      {run.status.kind === "interrupted" && (
        <Banner tone="canceled" title="Interrupted">
          Output below is partial; the session&rsquo;s state is kept.
        </Banner>
      )}
      {run.status.kind === "canceled" && (
        <Banner tone="canceled" title="Canceled">
          Output below is partial.
        </Banner>
      )}
      {output.truncated && (
        <p className="ui-meta" data-testid="research-output-truncated">
          Older output was dropped (output limit).
        </p>
      )}
      <div ref={viewer} className="exp-research__entries" role="region" tabIndex={0} aria-label={`Output of run ${run.id}`}>
        {output.entries.map((entry) => (
          <OutputEntryView key={`${run.id}:${entry.seq}`} entry={entry} run={run} />
        ))}
        {isActiveRun(run) && !run.queued && <Spinner size="sm" aria-label="Running" />}
      </div>
      <Provenance run={run} />
    </div>
  );
}

/** Long text shows a bounded preview; "Show more" reveals more on request. */
function LongText({ text, className, testId, stream }: { text: string; className: string; testId: string; stream?: string }) {
  const [shown, setShown] = useState(ENTRY_PREVIEW_CHARS);
  const visible = text.length > shown ? text.slice(0, shown) : text;
  const hidden = text.length - visible.length;
  return (
    <>
      <pre className={className} data-stream={stream} data-testid={testId}>
        {visible}
      </pre>
      {hidden > 0 && (
        <Button size="sm" variant="ghost" onPress={() => setShown((n) => n + 64 * 1024)}>
          Show more ({formatBytes(hidden)} hidden)
        </Button>
      )}
    </>
  );
}

const OutputEntryView = memo(function OutputEntryView({ entry, run }: { entry: OutputEntry; run: RunSummary }) {
  const item = entry.item;
  switch (item.kind) {
    case "stream":
      return (
        <LongText text={stripAnsi(item.text)} className="exp-research__stream" testId="research-stream" stream={item.name} />
      );
    case "display": {
      const { figure, table, html } = displayArtifacts(run, item.artifacts);
      if (figure) return <FigureView run={run} figure={figure} />;
      if (table && html) return <TableView run={run} table={table} html={html} />;
      return <LongText text={item.text ?? ""} className="exp-research__result" testId="research-result" />;
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
      return <p className="exp-research__notice ui-meta">{item.message}</p>;
    case "clearOutput":
      return null;
  }
});

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
      <figcaption className="ui-meta">
        {figure.path} · {figure.mediaType} · {formatBytes(figure.sizeBytes)}
      </figcaption>
      <RetainControls run={run} artifact={figure} />
      <PromotionControls run={run} artifact={figure} />
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
        <p className="ui-meta">The output contained no table.</p>
      ) : (
        <div className="exp-research__grid-scroll" role="region" tabIndex={0} aria-label="Table output">
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
          {grid.truncated && <p className="ui-meta">Showing 200 of {grid.totalRows} rows.</p>}
        </div>
      )}
      <RetainControls run={run} artifact={table} />
      <PromotionControls run={run} artifact={table} />
    </div>
  );
}

function codeOrigin(manifest: RunManifest): string {
  const origin = manifest.code.origin;
  return origin.kind === "input"
    ? "Research input"
    : origin.kind === "file"
      ? `File ${origin.path}${origin.revision !== null ? ` (revision ${origin.revision})` : ""}`
      : `Selection of ${origin.path ?? "an unsaved document"} (revision ${origin.revision})`;
}

function inputFact(manifest: RunManifest, input: RunManifest["record"]["inputs"][number]): KeyValueItem {
  const origin = manifest.code.origin;
  const unsaved = manifest.unsavedInputs.includes(input.path);
  const code = manifest.code.text !== null && origin.kind !== "input" && "path" in origin && origin.path === input.path;
  const hash = input.contentHash;
  return {
    key: `input:${input.path}`,
    label: "Input",
    valueProps: { "data-testid": "research-input", "data-path": input.path, "data-unsaved": unsaved ? "true" : "false" },
    value: (
      <>
        <code className="ui-code">{input.path}</code>
        {input.revision !== null && ` · revision ${input.revision}`}
        {hash && (
          <>
            {" · "}
            <code className="ui-code" title={hash}>
              {hash.slice(0, 23)}…
            </code>
          </>
        )}
        {unsaved &&
          (code
            ? " · unsaved buffer (its exact text is kept in the run manifest)"
            : " · unsaved buffer (only its digest is recorded)")}
      </>
    ),
  };
}

function artifactFact(artifact: ArtifactRecord): KeyValueItem {
  return {
    key: `artifact:${artifact.id}`,
    label: artifact.kind === "figure" ? "Figure" : artifact.kind === "table" ? "Table" : "Output",
    value: (
      <>
        <code className="ui-code">{artifact.path}</code> · {formatBytes(artifact.sizeBytes)} ·{" "}
        <code className="ui-code" title={artifact.contentHash}>
          {artifact.contentHash.slice(0, 23)}…
        </code>
      </>
    ),
  };
}

/** The run's manifest: code, inputs, kernel, interpreter, versions, and outputs (collapsed section). */
export function Provenance({ run }: { run: RunSummary }) {
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
  const { record, session } = manifest;
  const items: KeyValueItem[] = [
    { key: "manifest", label: "Manifest", value: `.exposition/runs/${record.id}.json`, code: true },
    {
      key: "code",
      label: "Code",
      value: (
        <>
          {codeOrigin(manifest)}
          {" · "}
          <code className="ui-code" title={manifest.code.sha256}>
            sha256 {manifest.code.sha256.slice(0, 16)}…
          </code>
        </>
      ),
    },
    ...(manifest.authorization
      ? [
          {
            key: "authorization",
            label: "Authorized",
            value: authorizationText(manifest.authorization),
            valueProps: { "data-testid": "research-authorization" },
          },
        ]
      : []),
    ...record.inputs.map((input) => inputFact(manifest, input)),
    {
      key: "kernel",
      label: "Kernel",
      value: `${session.kernel.implementation} ${session.kernel.implementationVersion} · ${session.kernel.language} ${session.kernel.languageVersion} · session generation ${session.generation}`,
    },
    {
      key: "interpreter",
      label: "Interpreter",
      value: (
        <>
          {session.interpreter.implementation} {session.interpreter.version} ·{" "}
          <code className="ui-code">{session.interpreter.location}</code>
        </>
      ),
    },
    { key: "versions", label: "Versions", value: record.environment.map((t) => `${t.name} ${t.version}`).join(" · ") },
    {
      key: "started",
      label: "Started",
      value: `${formatClock(record.startedAtMs)}${record.finishedAtMs !== null ? ` · finished ${formatClock(record.finishedAtMs)}` : ""}`,
    },
    ...record.outputs.map(artifactFact),
  ];
  return (
    <Section title="Provenance" collapsible defaultExpanded={false} className="exp-research__provenance">
      <div data-testid="research-provenance">
        <KeyValueList items={items} />
      </div>
    </Section>
  );
}

/** Operation attributes and handlers of one run row (`useOpList().itemProps`). */
export type RunOpProps = ReturnType<ReturnType<typeof useOpList<RunSummary>>["itemProps"]>;

/**
 * One run in a run list: its code (or `title`), quiet status, time, and
 * outputs. Click or Enter/Space selects. In a run history (`ops`) the row is
 * also an operation item: right click / Shift+F10 opens its menu, and Delete
 * acts on it while it has focus (XR-A-03).
 */
export function RunRow({
  run,
  now,
  selected,
  title,
  onSelect,
  ops,
}: {
  run: RunSummary;
  now: number;
  selected?: boolean;
  /** Replaces the code preview as the row title (e.g. the start time of an attempt). */
  title?: string;
  onSelect: () => void;
  ops?: RunOpProps;
}) {
  const pill = runPill(run);
  // Joined so a missing first fact never leaves a dangling separator.
  const facts = [
    title === undefined ? formatClock(run.startedAtMs) : null,
    run.owner.kind === "agent" ? "agent" : null,
    run.outputs.some((a) => a.kind === "figure") ? "figure" : null,
    run.outputs.some((a) => a.kind === "table") ? "table" : null,
  ].filter((f): f is string => f !== null);
  const onKeyDown = (event: KeyboardEvent) => {
    ops?.onKeyDown(event);
    if (event.key !== "Enter" && event.key !== " ") return;
    event.preventDefault();
    onSelect();
  };
  const onClick = (event: MouseEvent) => {
    ops?.onClick(event);
    // Ctrl/Cmd+click adds the run to the operation selection without showing it.
    if (ops && (event.ctrlKey || event.metaKey)) return;
    onSelect();
  };
  return (
    <li>
      <ListRow
        {...ops}
        className={ops ? `exp-research__run ${ops.className}` : "exp-research__run"}
        role="button"
        tabIndex={0}
        selected={selected}
        current={selected}
        aria-current={selected || undefined}
        data-run-id={run.id}
        codeTitle={title === undefined}
        title={title ?? (run.codePreview || "(empty)")}
        meta={
          <>
            <StatusText status={pill.status} label={pill.label} detail={formatDuration(run, now)} />
            {facts.length > 0 && <span>{facts.join(" · ")}</span>}
          </>
        }
        onClick={onClick}
        onKeyDown={onKeyDown}
      />
    </li>
  );
}

/** Rows the run history shows at first and adds with each "Show more runs". */
const HISTORY_ROWS = 200;
const count = (n: number) => n.toLocaleString("en-US");

export function RunHistory() {
  const runs = useResearch((s) => s.runs);
  const selected = useResearch((s) => s.selectedRun);
  const more = useResearch((s) => s.runsNext !== null);
  const remaining = useResearch((s) => s.runsRemaining);
  const unreadable = useResearch((s) => s.runsUnreadable);
  const loading = useResearch((s) => s.runsLoadingMore);
  const [rows, setRows] = useState(HISTORY_ROWS);
  // Another project (the list starts empty): the first rows again.
  useEffect(() => {
    if (runs.length === 0) setRows(HISTORY_ROWS);
  }, [runs.length]);
  const visible = useMemo(() => runs.slice(0, rows), [runs, rows]);
  // Not shown: loaded runs beyond the rows shown, and the records on pages
  // not read yet. Those are counted as records: one of them may turn out
  // not to be a run when its page is read, and is then counted in
  // `unreadable` instead, so the total stays what it was.
  const notShown = runs.length - visible.length + (more ? remaining : 0);
  const records = visible.length + notShown + unreadable;
  const showMore = () => {
    if (loading) return;
    setRows(rows + HISTORY_ROWS);
    if (rows + HISTORY_ROWS > runs.length && more) void loadMoreRuns();
  };
  const onMoreKeyDown = (event: KeyboardEvent) => {
    if (event.key !== "Enter" && event.key !== " ") return;
    event.preventDefault();
    showMore();
  };
  const now = useNow(runs.some(isActiveRun));
  // Run records and their outputs are deleted (recoverably) and restored here
  // as in the workflow run lists: menu, Delete key, palette, Undo.
  const list = useOpList({
    id: "research-runs",
    label: "Run actions",
    items: visible,
    keyOf: (r) => r.id,
    actionsFor: researchRunActions,
  });
  return (
    <div className="exp-research__history">
      <PanelHeader
        title="Runs"
        count={visible.length < records ? `${count(visible.length)} of ${count(records)} records` : undefined}
      />
      {visible.length === 0 ? (
        <EmptyState fill={false} title="No runs yet" />
      ) : (
        <div ref={list.ref} className="xp-ops-surface" data-testid="research-runs-surface">
          <ul className="exp-research__runs" aria-label="Run history" data-testid="research-history">
            {visible.map((run) => (
              <RunRow
                key={run.id}
                run={run}
                now={now}
                selected={run.id === selected}
                onSelect={() => void selectRun(run.id)}
                ops={list.itemProps(run.id)}
              />
            ))}
            {(visible.length < runs.length || more) && (
              <li>
                <ListRow
                  role="button"
                  tabIndex={0}
                  aria-busy={loading || undefined}
                  data-testid="research-history-more"
                  title="Show more runs"
                  meta={loading ? "Reading…" : notShown > 0 ? `${count(notShown)} ${notShown === 1 ? "record" : "records"} not shown` : undefined}
                  onClick={showMore}
                  onKeyDown={onMoreKeyDown}
                />
              </li>
            )}
            {unreadable > 0 && (
              <li>
                <ListRow
                  static
                  data-testid="research-history-unreadable"
                  title={unreadable === 1 ? "1 run record cannot be read" : `${count(unreadable)} run records cannot be read`}
                  meta="Not listed as runs"
                />
              </li>
            )}
          </ul>
        </div>
      )}
      {list.contextMenu}
    </div>
  );
}
