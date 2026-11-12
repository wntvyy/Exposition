// Lean infoview: checking status (exact toolchain and revision), goals at the
// cursor as plain text, incomplete declarations (sorry), and diagnostics for
// the active Lean file. No third-party widgets and no HTML from the server.
import { Download, FileCode2, RefreshCw, RotateCcw, ShieldCheck } from "lucide-react";
import { useEffect } from "react";

import type { Diagnostic } from "../../bindings/Diagnostic";
import type { LeanDocumentState } from "../../bindings/LeanDocumentState";
import { useActiveEditor } from "../../app/editors";
import { layout } from "../../app/layout";
import { notify } from "../../app/notifications";
import { commands, errorMessage, isCommandError } from "../../lib/ipc";
import {
  Banner,
  Button,
  EmptyState,
  IconButton,
  ProgressBar,
  Section,
  Spinner,
  StatusPill,
  Toolbar,
  confirm,
} from "../../ui/components";
import { handleFor } from "../editor";
import { requestWorkspaceTrust } from "../jobs/trust";
import { CheckedDeclarations } from "./CheckedDeclarations";
import { AuditedDeclarations, useDeclarationEvidence } from "./FormalEvidence";
import { revealInEditor } from "./leanMonaco";
import { statusSentence, statusView } from "./status";
import { startLeanTracking, useLean } from "./store";

export const LEAN_VIEW_ID = "lean.infoview";
/** The infoview lists at most this many diagnostics (all are markers in Monaco). */
const MAX_DIAGNOSTICS_SHOWN = 200;

function lineOf(text: string | undefined, offset: number): number {
  // Offsets are UTF-16 code units, like JavaScript strings.
  return text ? text.slice(0, offset).split("\n").length : 0;
}

/**
 * Ask to trust the folder. The Lean service follows every grant (from here or
 * any other view) and starts the server itself; restarting here would only
 * stop the server it just started.
 */
export async function trustAndRestart(state: LeanDocumentState) {
  if (state.status.kind !== "untrusted") return;
  await requestWorkspaceTrust(state.status.workspaceRoot);
}

export async function installToolchain(state: LeanDocumentState) {
  if (state.status.kind !== "toolchainMissing" || !state.status.install) return;
  const toolchain = state.status.install;
  const choice = await confirm<"cancel" | "install">({
    title: `Install Lean toolchain ${toolchain}?`,
    message: `elan downloads ${toolchain} from the internet and installs it for your user account. The project is then checked with exactly that version.`,
    actions: [
      { id: "cancel", label: "Cancel", variant: "secondary" },
      { id: "install", label: "Install toolchain", variant: "primary" },
    ],
    cancelId: "cancel",
    defaultId: "cancel",
    tone: "warning",
  });
  if (choice !== "install") return;
  try {
    await commands.leanInstallToolchain(state.document);
    notify({
      tone: "info",
      title: `Installing ${toolchain}`,
      body: "Follow progress in the Jobs panel. Checking starts when the installation finishes.",
      actions: [{ label: "Show Jobs", run: () => layout.showView("panel", "jobs.panel") }],
    });
  } catch (e) {
    if (isCommandError(e) && e.code === "lean.untrusted") {
      const root = state.projectRoot ?? "";
      if (root && (await requestWorkspaceTrust(root))) return installToolchain(state);
      return;
    }
    notify({ tone: "danger", title: "Could not install the toolchain", body: errorMessage(e) });
  }
}

function GoalText({ text }: { text: string }) {
  const lines = text.split("\n");
  const turnstile = lines.findIndex((l) => l.startsWith("⊢"));
  const hyps = turnstile < 0 ? lines : lines.slice(0, turnstile);
  const target = turnstile < 0 ? [] : lines.slice(turnstile);
  return (
    <pre className="exp-lean__goal" data-testid="lean-goal">
      {hyps.length > 0 && <span className="exp-lean__hyps">{hyps.join("\n")}</span>}
      {hyps.length > 0 && target.length > 0 && "\n"}
      {target.length > 0 && <span className="exp-lean__target">{target.join("\n")}</span>}
    </pre>
  );
}

function GoalsSection({ state }: { state: LeanDocumentState }) {
  const cursor = useLean((s) => s.cursor);
  const view = useLean((s) => s.goals);
  const here = cursor?.document === state.document;
  let body;
  if (!here) {
    body = <p className="exp-muted">Place the cursor in this file to see its goals.</p>;
  } else if (!view) {
    body = <p className="exp-muted">Goals appear once Lean has checked up to the cursor.</p>;
  } else if (view.error) {
    body = <p className="exp-lean__error" role="alert">{view.error}</p>;
  } else {
    const goals = view.goals?.goals;
    const term = view.goals?.termGoal;
    body = (
      <>
        {goals == null && !term && !view.loading && <p className="exp-muted" data-testid="lean-no-goals">No goals here (not inside a tactic proof).</p>}
        {goals && goals.length === 0 && <p data-testid="lean-goals-done"><StatusPill size="sm" status="succeeded" label="No goals" /> No goals remain at this point.</p>}
        {goals?.map((g, i) => (
          <div key={i} className="exp-lean__goal-block">
            {goals.length > 1 && <div className="exp-lean__goal-label">Goal {i + 1} of {goals.length}</div>}
            <GoalText text={g} />
          </div>
        ))}
        {term && (
          <div className="exp-lean__goal-block" data-testid="lean-term-goal">
            <div className="exp-lean__goal-label">Expected type</div>
            <GoalText text={term.goal} />
          </div>
        )}
      </>
    );
  }
  return (
    <Section
      title="Goals"
      actions={
        here && cursor ? (
          <span className="exp-lean__where">
            {view?.loading && <Spinner size="sm" aria-label="Loading goals" />} Ln {cursor.line}, Col {cursor.column}
          </span>
        ) : undefined
      }
    >
      <div className="exp-lean__goals" data-testid="lean-goals" aria-live="polite" aria-busy={view?.loading ?? false}>
        {body}
      </div>
    </Section>
  );
}

function DiagnosticRow({ d, state, text }: { d: Diagnostic; state: LeanDocumentState; text?: string }) {
  const line = d.location ? lineOf(text, d.location.range.start) : null;
  const pill = d.severity === "error" ? "failed" : d.severity === "warning" ? "warning" : "idle";
  const label = d.hints.some((h) => h.startsWith("Incomplete")) ? "sorry" : d.severity;
  return (
    <li className="exp-lean__diag" data-severity={d.severity}>
      <button
        type="button"
        className="exp-lean__diag-jump"
        disabled={!d.location}
        onClick={() => d.location && void revealInEditor(state.path, { range: d.location.range })}
        aria-label={line ? `Go to line ${line}` : "No location"}
      >
        <StatusPill size="sm" status={pill} label={label} />
        {line != null && <span className="exp-lean__line">Ln {line}</span>}
      </button>
      <pre className="exp-lean__message">{d.message}</pre>
    </li>
  );
}

export function LeanPanel() {
  useEffect(() => startLeanTracking(), []);
  const active = useActiveEditor();
  const path = active?.resource.path;
  const state = useLean((s) => (path ? Object.values(s.docs).find((d) => d.path === path) : undefined));
  return <LeanInfoview path={path} state={state} />;
}

function LeanInfoview({ path, state }: { path: string | undefined; state: LeanDocumentState | undefined }) {
  if (!path || !path.endsWith(".lean")) {
    return (
      <EmptyState
        icon={FileCode2}
        title="No Lean file"
        description="Open a .lean file to see its checking status, goals at the cursor, and diagnostics."
      />
    );
  }
  if (!state) {
    return <EmptyState icon={FileCode2} title="Waiting for Lean" description={`${path} is not tracked by Lean yet.`} />;
  }
  return <LeanDocumentView state={state} />;
}

function LeanDocumentView({ state }: { state: LeanDocumentState }) {
  const formal = useDeclarationEvidence(state);
  const status = state.status;
  const view = statusView(status);
  const toolchain = state.toolchain;
  const sorries = status.kind === "checkedWithSorry" ? status.locations : [];
  const axiomUses =
    status.kind === "checkedWithAxioms" || status.kind === "reliesOnCompiledCode" ? status.declarations : [];
  const axioms =
    status.kind === "checked"
      ? status.axioms
      : status.kind === "checkedWithAxioms" || status.kind === "reliesOnCompiledCode"
        ? status.axioms
        : null;
  const shown = state.diagnostics.slice(0, MAX_DIAGNOSTICS_SHOWN);
  const stale = state.diagnosticsRevision != null && state.diagnosticsRevision !== state.revision;
  const text = modelText(state);

  return (
    <div className="exp-lean" data-testid="lean-panel" data-status={status.kind} data-revision={state.revision}>
      <div className="exp-lean__header">
        <div className="exp-lean__title">
          <span data-testid="lean-status">
            <StatusPill status={view.status} label={view.label} detail={view.detail} />
          </span>
          <span className="exp-lean__file" title={state.path}>{state.path.split("/").pop()}</span>
        </div>
        <Toolbar aria-label="Lean actions" className="exp-lean__actions">
          <IconButton icon={RotateCcw} aria-label="Restart file (rebuild imports)" onPress={() => void commands.leanRestartFile(state.document)} />
          <IconButton icon={RefreshCw} aria-label="Restart Lean server" onPress={() => void commands.leanRestart(state.document)} />
        </Toolbar>
      </div>
      <p className="exp-lean__sentence" data-testid="lean-status-sentence">{statusSentence(status)}</p>
      <dl className="exp-lean__facts">
        <dt>Revision</dt>
        <dd data-testid="lean-revision">{state.revision}</dd>
        <dt>Toolchain</dt>
        <dd data-testid="lean-toolchain">
          {toolchain ? (
            <>
              <code>{toolchain.name}</code>
              {toolchain.version && <div className="exp-muted">{toolchain.version}</div>}
              {toolchain.note && <div className="exp-muted">{toolchain.note}</div>}
            </>
          ) : status.kind === "toolchainMissing" ? (
            <code>{status.toolchain}</code>
          ) : (
            "—"
          )}
        </dd>
        {axioms && (
          <>
            <dt>Axioms</dt>
            <dd data-testid="lean-axioms">
              {axioms.length ? axioms.map((a) => <code key={a}>{a} </code>) : "none"}
            </dd>
          </>
        )}
      </dl>

      {status.kind === "checking" && (
        <ProgressBar
          label={
            status.starting
              ? "Starting the Lean server"
              : status.auditing
                ? `Auditing the axioms of revision ${status.revision}`
                : `Checking revision ${status.revision}`
          }
          valueLabel={status.processing[0] ? `from line ${lineOf(text, status.processing[0].start)}` : undefined}
        />
      )}
      {status.kind === "untrusted" && (
        <Banner
          tone="warning"
          title="Workspace not trusted"
          actions={<Button size="sm" variant="primary" icon={ShieldCheck} onPress={() => void trustAndRestart(state)}>Trust workspace…</Button>}
        >
          Lean has not run. Checking executes the project’s code.
        </Banner>
      )}
      {status.kind === "toolchainMissing" && (
        <Banner
          tone="warning"
          title={`Toolchain ${status.toolchain} is not installed`}
          actions={
            status.install && (
              <Button size="sm" variant="primary" icon={Download} onPress={() => void installToolchain(state)}>
                Install toolchain {status.install}
              </Button>
            )
          }
        >
          Nothing is installed automatically.
        </Banner>
      )}
      {status.kind === "serverCrashed" && (
        <Banner
          tone="danger"
          title={status.restarting ? "Lean server crashed — restarting" : "Lean server crashed"}
          actions={<Button size="sm" onPress={() => void commands.leanRestart(state.document)}>Restart server</Button>}
        >
          <pre className="exp-lean__message">{status.message}</pre>
        </Banner>
      )}
      {status.kind === "unavailable" && <Banner tone="warning" title="Lean cannot check this file">{status.reason}</Banner>}
      {status.kind === "serverUnavailable" && (
        <Banner
          tone="warning"
          title="The Lean server has not started"
          actions={<Button size="sm" onPress={() => void commands.leanRestart(state.document)}>Retry now</Button>}
        >
          {status.reason}
        </Banner>
      )}
      {status.kind === "partiallyChecked" && (
        <Banner tone="warning" title="Only part of this file was checked">
          {status.reason}
        </Banner>
      )}
      {status.kind === "unverified" && (
        <Banner tone="warning" title="Not a verified result">
          {status.reason}
        </Banner>
      )}

      <GoalsSection state={state} />

      {sorries.length > 0 && (
        <Section title={`Incomplete (${sorries.length})`}>
          <ul className="exp-lean__list" data-testid="lean-sorries">
            {sorries.map((s, i) => (
              <li key={i}>
                <button type="button" className="exp-lean__diag-jump" onClick={() => void revealInEditor(state.path, { range: s.tokens[0] ?? s.range })}>
                  <StatusPill size="sm" status="warning" label="sorry" />
                  <span className="exp-lean__line">Ln {s.line + 1}</span>
                  <code>{s.declaration ?? "declaration"}</code>
                  {s.source === "axiomAudit" && <span className="exp-muted">found by the axiom audit</span>}
                </button>
              </li>
            ))}
          </ul>
        </Section>
      )}

      {status.kind === "checked" && (
        <CheckedDeclarations
          state={state}
          declarations={status.declarations}
          total={status.declarationCount}
          audit={formal?.audit ?? null}
          evidence={formal?.declarations}
        />
      )}
      {status.kind !== "checked" && formal?.audit && (
        <AuditedDeclarations state={state} audit={formal.audit} evidence={formal.declarations} />
      )}

      {axiomUses.length > 0 && (
        <Section title={status.kind === "reliesOnCompiledCode" ? `Trusts compiled code (${axiomUses.length})` : `Uses axioms (${axiomUses.length})`}>
          <ul className="exp-lean__list" data-testid="lean-axiom-uses">
            {axiomUses.map((u, i) => (
              <li key={i}>
                <button
                  type="button"
                  className="exp-lean__diag-jump"
                  disabled={!u.range}
                  onClick={() => u.range && void revealInEditor(state.path, { range: u.range })}
                >
                  <StatusPill size="sm" status="warning" label="axiom" />
                  {u.line != null && <span className="exp-lean__line">Ln {u.line + 1}</span>}
                  <code>{u.declaration}</code>
                </button>
                <pre className="exp-lean__message">{u.axioms.join(", ")}</pre>
              </li>
            ))}
          </ul>
        </Section>
      )}

      <Section title={`Diagnostics (${state.diagnostics.length})`}>
        {stale && (
          <Banner tone="stale" title={`From revision ${state.diagnosticsRevision}`}>
            Lean is checking revision {state.revision}; these may change.
          </Banner>
        )}
        {state.diagnostics.length === 0 ? (
          <p className="exp-muted">{view.status === "running" ? "None yet." : "No diagnostics."}</p>
        ) : (
          <ul className="exp-lean__list" data-testid="lean-diagnostics">
            {shown.map((d, i) => (
              <DiagnosticRow key={i} d={d} state={state} text={text} />
            ))}
          </ul>
        )}
        {state.diagnostics.length > shown.length && (
          <p className="exp-muted" data-testid="lean-diagnostics-more">
            {state.diagnostics.length - shown.length} more not shown here; they are marked in the editor.
          </p>
        )}
      </Section>
    </div>
  );
}

/** Text of the document as its editor holds it (for line numbers in this view). */
function modelText(state: LeanDocumentState): string | undefined {
  const handle = handleFor(state.document);
  return handle && !handle.closed ? handle.model.getValue() : undefined;
}
