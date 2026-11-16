// Guided Mathlib setup: the plan (exactly what will be created, kept, and
// run, before anything runs), progress with Cancel, and an honest outcome.
// A new Mathlib package or the pinned dependencies and prebuilt cache of an
// existing one; never an upgrade. Rust chooses the toolchain and the Mathlib
// revision and builds every command; this view only shows them.
import { useCallback, useEffect, useRef, useState } from "react";
import type React from "react";
import { Hammer, PackageCheck, RefreshCw, ShieldAlert, Square, Wrench } from "lucide-react";

import type { LeanDocumentState } from "../../bindings/LeanDocumentState";
import type { LeanPackageInspection } from "../../bindings/LeanPackageInspection";
import type { LeanProjectInfo } from "../../bindings/LeanProjectInfo";
import type { LeanSetupPlan } from "../../bindings/LeanSetupPlan";
import type { LeanSetupRequest } from "../../bindings/LeanSetupRequest";
import type { LeanSetupSource } from "../../bindings/LeanSetupSource";
import type { LeanSetupStatus } from "../../bindings/LeanSetupStatus";
import { commands, errorMessage, isCommandError } from "../../lib/ipc";
import {
  Badge,
  Banner,
  Button,
  Dialog,
  KeyValueList,
  LoadingState,
  ProgressBar,
  Section,
  Select,
  SimpleTable,
  StatusPill,
  TextField,
  confirm,
  type KeyValueItem,
} from "../../ui/components";
import { requestWorkspaceTrust } from "../jobs/trust";
import {
  cancelLeanSetup,
  closeMathlibSetup,
  commandLine,
  dismissLeanSetupOutcome,
  folderLabel,
  openMathlibSetup,
  startLeanSetup,
  startLeanSetupTracking,
  useLeanSetup,
  type MathlibSetupTarget,
} from "./setup";

const NEW_PACKAGE = "\u0000new";
/** Codes of a plan refused for what was typed (shown on the field). */
const FOLDER_CODES = ["lean.setup.invalidFolder", "lean.setup.insidePackage", "lean.setup.containsPackage", "lean.setup.packageExists"];
const NAME_CODES = ["lean.setup.invalidName", "lean.setup.wouldOverwrite"];

type PlanLoad =
  | { kind: "idle" }
  | { kind: "loading" }
  | { kind: "ready"; plan: LeanSetupPlan }
  | { kind: "error"; code: string; message: string };

function requestFor(choice: string, folder: string, name: string): LeanSetupRequest {
  return choice === NEW_PACKAGE
    ? { kind: "newMathlibPackage", folder: folder.trim(), name: name.trim() }
    : { kind: "prepareMathlibPackage", folder: choice };
}

function planFacts(plan: LeanSetupPlan): KeyValueItem[] {
  const files = (names: string[]) =>
    names.map((name, i) => (
      <span key={name}>
        {i > 0 && ", "}
        <code className="ui-code">{name}</code>
      </span>
    ));
  const facts: KeyValueItem[] = [
    {
      key: "package",
      label: "Package folder",
      value: (
        <>
          <code className="ui-code">{plan.packageRoot}</code>{" "}
          {plan.createsPackage ? "— a new Lake package" : "— the existing Lake package"}
        </>
      ),
      valueProps: { "data-testid": "lean-setup-folder" },
    },
    {
      key: "toolchain",
      label: "Lean",
      value: (
        <>
          <code className="ui-code">{plan.toolchain}</code>{" "}
          {plan.toolchainInstalled ? "— installed" : "— installed by this setup for your user account (elan)"}. elan&rsquo;s
          default toolchain is not changed.
        </>
      ),
      valueProps: { "data-testid": "lean-setup-toolchain" },
    },
    {
      key: "mathlib",
      label: "Mathlib",
      value: (
        <>
          <code className="ui-code">{plan.mathlibRevision}</code>{" "}
          {plan.createsPackage
            ? "— the release made for this Lean version"
            : plan.pinnedByManifest
              ? "— the revision lake-manifest.json pins; nothing is upgraded"
              : "— the revision the lakefile requires; resolved once into lake-manifest.json"}
        </>
      ),
      valueProps: { "data-testid": "lean-setup-mathlib" },
    },
  ];
  if (plan.writes.length > 0) facts.push({ key: "writes", label: "Creates", value: files(plan.writes) });
  if (plan.keeps.length > 0) facts.push({ key: "keeps", label: "Keeps unchanged", value: files(plan.keeps) });
  facts.push(
    {
      key: "downloads",
      label: "Downloads",
      value:
        "The dependencies from the addresses listed under Sources, and Mathlib's prebuilt cache from cache.mathlib.org: several GB, into the package's .lake folder.",
    },
    {
      key: "runs",
      label: "Runs",
      value: (
        <>
          <Badge tone="warning" icon={ShieldAlert}>
            On this computer with your account&rsquo;s privileges
          </Badge>{" "}
          Lake runs the package&rsquo;s configuration and downloaded programs.
        </>
      ),
    },
    {
      key: "recovery",
      label: "If it fails",
      value: plan.createsPackage
        ? "The new package's files are removed; nothing else in the project changes. A file you add or change in the new folder meanwhile is kept."
        : "Your configuration files stay as they are. The package is not shown as ready until a setup of it succeeds.",
    },
  );
  return facts;
}

/** Where a dependency comes from, in words. */
function sourceOrigin(source: LeanSetupSource): React.ReactNode {
  if (source.url) return <code className="ui-code">{source.url}</code>;
  if (source.kind === "path") return "A local folder";
  if (source.kind === "reservoir") {
    return (
      <>
        The Lean package registry (Reservoir) entry{" "}
        <code className="ui-code">{source.scope ? `${source.scope}/${source.name}` : source.name}</code>
      </>
    );
  }
  return "Not stated";
}

function sourceRevision(source: LeanSetupSource): React.ReactNode {
  if (source.rev) {
    return (
      <>
        <code className="ui-code" title={source.rev}>
          {source.rev.slice(0, 12)}
        </code>
        {source.inputRev ? ` (${source.inputRev})` : ""}
      </>
    );
  }
  return source.inputRev ? <code className="ui-code">{source.inputRev}</code> : "Chosen by Lake";
}

/** The row of the dependency whose `cache` program the setup runs. */
function isMathlib(source: LeanSetupSource): boolean {
  return source.name.toLowerCase() === "mathlib";
}

/** Every dependency of the plan with the address and revision it is fetched from. */
function PlanSources({ plan }: { plan: LeanSetupPlan }) {
  const fork = plan.mathlibSource.official ? null : plan.mathlibSource.url;
  return (
    <Section title="Sources" count={plan.sources.length}>
      {fork && (
        <Banner tone="warning" title="Mathlib is not from the official repository" data-testid="lean-setup-fork-warning">
          Mathlib comes from <code className="ui-code">{fork}</code>, not the official repository (
          <code className="ui-code">https://github.com/leanprover-community/mathlib4</code>). This setup downloads and runs that
          repository&rsquo;s code: its <code className="ui-code">cache</code> program runs on this computer with your account&rsquo;s
          privileges. The prebuilt cache may not cover a fork; then the cache step or the check after it fails, and Mathlib can
          only be built from source (hours).
        </Banner>
      )}
      <SimpleTable<LeanSetupSource>
        aria-label="Where the dependencies are fetched from"
        data-testid="lean-setup-sources"
        columns={[
          { id: "name", title: "Dependency", isRowHeader: true, render: (source) => source.name },
          {
            id: "from",
            title: "Fetched from",
            render: (source) => (
              <>
                {sourceOrigin(source)}
                {fork && isMathlib(source) && (
                  <>
                    {" "}
                    <Badge tone="warning">Not official</Badge>
                  </>
                )}
              </>
            ),
          },
          { id: "revision", title: "Revision", width: "24%", render: sourceRevision },
          {
            id: "state",
            title: "State",
            width: "20%",
            render: (source) => (source.present ? "Already downloaded" : plan.pinnedByManifest ? "Fetched by this setup" : "Resolved by Lake"),
          },
        ]}
        rows={plan.sources}
        rowKey={(source) => source.name}
        rowProps={(source) => ({
          "data-source": source.name,
          "data-url": source.url ?? undefined,
          "data-official": isMathlib(source) ? String(plan.mathlibSource.official) : undefined,
        })}
        empty="The lakefile names no requirement that could be read."
      />
      {plan.sourcesNote && (
        <p className="ui-meta" data-testid="lean-setup-sources-note">
          {plan.sourcesNote}
        </p>
      )}
    </Section>
  );
}

/** A running setup: step, live output, Cancel. */
export function MathlibSetupProgress({ status }: { status: LeanSetupStatus }) {
  const log = useLeanSetup((s) => s.log);
  const viewer = useRef<HTMLPreElement>(null);
  useEffect(() => {
    const el = viewer.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [log]);
  const steps = status.plan.steps.length;
  const step = status.step ?? 0;
  const label =
    status.phase === "cleaningUp"
      ? "Removing what this setup created"
      : status.phase === "verifying"
        ? "Checking the result"
        : status.phase === "preparing"
          ? "Preparing"
          : `Step ${step + 1} of ${steps}: ${status.plan.steps[step]?.label ?? ""}`;
  return (
    <div className="exp-lean-setup__progress" data-testid="lean-setup-progress" data-phase={status.phase} data-step={status.step ?? undefined}>
      <Banner
        tone="info"
        title={`Setting up Mathlib in ${folderLabel(status.plan.folder)}`}
        actions={
          <Button size="sm" icon={Square} onPress={() => void cancelLeanSetup()} data-testid="lean-setup-cancel">
            Cancel setup
          </Button>
        }
      >
        <ProgressBar label={label} />
        {log.length > 0 && (
          <pre ref={viewer} className="exp-lean-setup__log" tabIndex={0} aria-label="Setup output" data-testid="lean-setup-log">
            {log.slice(-12).join("\n")}
          </pre>
        )}
      </Banner>
    </div>
  );
}

/** A finished setup: what happened, and what to do next. */
export function MathlibSetupOutcome({ status, onRetry }: { status: LeanSetupStatus; onRetry?: () => void }) {
  const log = useLeanSetup((s) => s.log);
  const [showLog, setShowLog] = useState(false);
  const where = folderLabel(status.plan.folder);
  if (status.state === "succeeded") {
    return (
      <div data-testid="lean-setup-outcome" data-state="succeeded">
        <Banner tone="success" title="Mathlib is ready">
          The package in {where} uses Lean <code className="ui-code">{status.plan.toolchain}</code> and Mathlib{" "}
          <code className="ui-code">{status.inspection?.mathlibRevision ?? status.plan.mathlibRevision}</code>. Open a Lean file
          in it to check it. The prebuilt cache only saves build time; a file is verified by Lean&rsquo;s check and audit.
        </Banner>
      </div>
    );
  }
  const canceled = status.state === "canceled";
  return (
    <div data-testid="lean-setup-outcome" data-state={status.state} data-removed={status.removed.length} data-kept={status.kept.length}>
      <Banner
        tone={canceled ? "canceled" : "danger"}
        title={canceled ? "Setup canceled" : "Setup failed"}
        actions={
          <>
            {onRetry && (
              <Button size="sm" icon={Wrench} onPress={onRetry} data-testid="lean-setup-retry">
                Set up again…
              </Button>
            )}
            {log.length > 0 && (
              <Button size="sm" variant="ghost" onPress={() => setShowLog((v) => !v)} aria-expanded={showLog}>
                {showLog ? "Hide output" : "Show output"}
              </Button>
            )}
          </>
        }
      >
        <span data-testid="lean-setup-outcome-message">{status.message}</span>
        {showLog && (
          <pre className="exp-lean-setup__log" tabIndex={0} aria-label="Setup output">
            {log.join("\n")}
          </pre>
        )}
      </Banner>
    </div>
  );
}

/** The dialog while planning: choose the package, review the plan, start. Inactive while a setup is shown. */
function usePlanView(target: MathlibSetupTarget, active: boolean, onStarted: () => void): { body: React.ReactNode; footer: React.ReactNode } {
  const [projects, setProjects] = useState<LeanProjectInfo[] | null>(null);
  const [choice, setChoice] = useState<string | null>(null);
  const [folder, setFolder] = useState("lean");
  const [name, setName] = useState("Formal");
  const [load, setLoad] = useState<PlanLoad>({ kind: "idle" });
  const [starting, setStarting] = useState(false);
  const [failure, setFailure] = useState<{ code: string; message: string } | null>(null);
  const serial = useRef(0);

  const targetKey = JSON.stringify(target);
  useEffect(() => {
    if (!active) return;
    let live = true;
    setChoice(null);
    setLoad({ kind: "idle" });
    void commands
      .leanProjects()
      .catch(() => [] as LeanProjectInfo[])
      .then((all) => {
        if (!live) return;
        const packages = all.filter((p) => p.kind.kind === "lake");
        setProjects(packages);
        const wanted = JSON.parse(targetKey) as MathlibSetupTarget;
        if (wanted.kind === "prepare" && packages.some((p) => (p.path ?? "") === wanted.folder)) setChoice(wanted.folder);
        else if (wanted.kind === "auto" && packages[0]) setChoice(packages[0].path ?? "");
        else setChoice(NEW_PACKAGE);
      });
    return () => {
      live = false;
    };
  }, [targetKey, active]);

  const request = choice === null || !active ? null : requestFor(choice, folder, name);
  const requestKey = JSON.stringify(request);
  const fetchPlan = useCallback(async () => {
    const mine = ++serial.current;
    const current = JSON.parse(requestKey) as LeanSetupRequest | null;
    if (!current) return;
    setLoad({ kind: "loading" });
    try {
      const plan = await commands.leanSetupPlan(current);
      if (mine === serial.current) setLoad({ kind: "ready", plan });
    } catch (e) {
      if (mine === serial.current) setLoad({ kind: "error", code: isCommandError(e) ? e.code : "error", message: errorMessage(e) });
    }
  }, [requestKey]);
  useEffect(() => {
    setFailure(null);
    const timer = setTimeout(() => void fetchPlan(), 300);
    return () => clearTimeout(timer);
  }, [fetchPlan]);

  const plan = load.kind === "ready" ? load.plan : null;
  const isNew = choice === NEW_PACKAGE;
  const folderError = load.kind === "error" && isNew && FOLDER_CODES.includes(load.code) ? load.message : null;
  const nameError = load.kind === "error" && isNew && NAME_CODES.includes(load.code) ? load.message : null;
  const prepared = load.kind === "error" && load.code === "lean.setup.alreadyPrepared";
  const stale = failure?.code === "lean.setup.planStale";

  const start = async () => {
    if (!plan) return;
    setStarting(true);
    setFailure(null);
    try {
      try {
        await startLeanSetup(plan);
      } catch (e) {
        if (!(isCommandError(e) && e.code === "lean.untrusted") || !(await requestWorkspaceTrust(plan.root))) throw e;
        await startLeanSetup(plan);
      }
      onStarted();
    } catch (e) {
      setFailure({ code: isCommandError(e) ? e.code : "error", message: errorMessage(e) });
    } finally {
      setStarting(false);
    }
  };

  const options = [
    ...(projects ?? []).map((p) => ({ id: p.path ?? "", label: `Existing package in ${folderLabel(p.path ?? "")}` })),
    { id: NEW_PACKAGE, label: "New package…" },
  ];
  const body = (
      <div className="exp-lean-setup" data-testid="lean-setup-plan" data-state={load.kind} data-plan-hash={plan?.planHash}>
        {projects === null || choice === null ? (
          <LoadingState title="Looking for Lean packages…" fill={false} />
        ) : (
          <>
            {options.length > 1 && (
              <Select<string> label="Package" options={options} value={choice} onChange={(id) => id !== null && setChoice(id)} wrapValue />
            )}
            {isNew && (
              <>
                <div data-testid="lean-setup-folder-field">
                  <TextField
                    label="Folder"
                    description="Inside the project. Leave empty to make the project folder itself the package."
                    value={folder}
                    onChange={setFolder}
                    isInvalid={folderError !== null}
                    errorMessage={folderError ?? undefined}
                    code
                  />
                </div>
                <div data-testid="lean-setup-name-field">
                  <TextField
                    label="Library name"
                    description="Your Lean files go in a folder with this name, for example Formal or OddSums."
                    value={name}
                    onChange={setName}
                    isInvalid={nameError !== null}
                    errorMessage={nameError ?? undefined}
                    code
                  />
                </div>
              </>
            )}
            {load.kind === "loading" && !plan && <LoadingState title="Preparing the plan…" fill={false} />}
            {plan && (
              <div data-testid="lean-setup-summary">
                <KeyValueList items={planFacts(plan)} />
              </div>
            )}
            {plan && <PlanSources plan={plan} />}
            {prepared && (
              <Banner tone="success" title="Already prepared">
                <span data-testid="lean-setup-plan-error" data-code="lean.setup.alreadyPrepared">
                  {load.kind === "error" ? load.message : ""}
                </span>
              </Banner>
            )}
            {load.kind === "error" && !folderError && !nameError && !prepared && (
              <Banner tone="danger" title="This setup cannot run">
                <span data-testid="lean-setup-plan-error" data-code={load.code}>
                  {load.message}
                </span>
              </Banner>
            )}
            {failure && (
              <div data-testid="lean-setup-start-error" data-code={failure.code}>
                <Banner
                  tone={stale ? "stale" : "danger"}
                  title={stale ? "The project changed since this plan was made" : "Setup did not start"}
                  actions={
                    stale ? (
                      <Button size="sm" icon={RefreshCw} onPress={() => void fetchPlan().then(() => setFailure(null))}>
                        Review the new plan
                      </Button>
                    ) : undefined
                  }
                >
                  {failure.message} Nothing was changed.
                </Banner>
              </div>
            )}
            {plan && (
              <Section title="Commands" count={plan.steps.length} collapsible defaultExpanded={false}>
                <pre className="exp-lean-setup__commands" data-testid="lean-setup-commands" tabIndex={0} aria-label="Commands the setup runs">
                  {plan.steps
                    .map((step, i) => {
                      const env = Object.entries(step.environment)
                        .map(([key, value]) => `${key}=${value} `)
                        .join("");
                      return `${i + 1}. ${step.label}${step.network ? " (downloads)" : ""}\n   ${env}${commandLine(step.program, step.args)}`;
                    })
                    .join("\n")}
                </pre>
                <p className="ui-meta">Each runs directly (no shell) in the package folder, one after the other. No other command runs.</p>
              </Section>
            )}
          </>
        )}
      </div>
  );
  const footer = (
    <>
        <Button slot="close" isDisabled={starting}>
          {prepared ? "Close" : "Cancel"}
        </Button>
        <Button
          variant="primary"
          icon={isNew ? Wrench : PackageCheck}
          isPending={starting}
          isDisabled={!plan || starting || stale}
          onPress={() => void start()}
          data-testid="lean-setup-start"
        >
          {isNew ? "Create package" : "Prepare package"}
        </Button>
    </>
  );
  return { body, footer };
}

/**
 * The Set Up Mathlib dialog. While a setup runs (or its outcome has not been
 * dismissed) it shows that setup; otherwise the plan. Closing it never stops
 * a running setup: the status bar keeps showing it and the command reopens it.
 */
export function MathlibSetupDialog() {
  const target = useLeanSetup((s) => s.dialog);
  const status = useLeanSetup((s) => s.status);
  const dismissed = useLeanSetup((s) => s.dismissed);
  const [planning, setPlanning] = useState(false);
  useEffect(() => startLeanSetupTracking(), []);
  const isOpen = target !== null;
  useEffect(() => {
    if (!isOpen) setPlanning(false);
  }, [isOpen]);
  const running = status?.state === "running" ? status : null;
  const outcome = status && status.state !== "running" && !dismissed.includes(status.id) ? status : null;
  const showRun = !planning && (running !== null || outcome !== null);
  const again = () => {
    if (outcome) dismissLeanSetupOutcome(outcome.id);
    setPlanning(true);
  };
  const planView = usePlanView(planning || target === null ? { kind: "auto" } : target, isOpen && !showRun, () => setPlanning(false));
  return (
    <Dialog
      title="Set up Mathlib"
      size="lg"
      body="content"
      isOpen={isOpen}
      onOpenChange={(open) => {
        if (!open) {
          if (outcome && outcome.state === "succeeded") dismissLeanSetupOutcome(outcome.id);
          closeMathlibSetup();
        }
      }}
      footer={
        showRun ? (
          <>
            {outcome && outcome.state === "succeeded" && (
              <Button onPress={again} data-testid="lean-setup-another">
                Set up another package…
              </Button>
            )}
            <Button slot="close" variant={running ? "secondary" : "primary"} data-testid="lean-setup-close">
              {running ? "Hide" : "Close"}
            </Button>
          </>
        ) : (
          planView.footer
        )
      }
    >
      {showRun ? (
        <div className="exp-lean-setup" data-testid="lean-setup-run" data-state={status?.state}>
          {running && <MathlibSetupProgress status={running} />}
          {running && <p className="ui-meta">Hiding this dialog does not stop the setup. Reopen it with the command Set Up Mathlib.</p>}
          {outcome && <MathlibSetupOutcome status={outcome} onRetry={again} />}
        </div>
      ) : (
        planView.body
      )}
    </Dialog>
  );
}

/** Status bar: the running Mathlib setup (it continues while its dialog is hidden). Also hosts the dialog. */
export function MathlibSetupStatusItem() {
  useEffect(() => startLeanSetupTracking(), []);
  const status = useLeanSetup((s) => s.status);
  const running = status?.state === "running" ? status : null;
  const steps = running?.plan.steps.length ?? 0;
  return (
    <>
      {running && (
        <span data-testid="lean-setup-status-item" data-phase={running.phase}>
          <StatusPill
            size="sm"
            status="running"
            label={running.phase === "step" ? `Mathlib setup: step ${(running.step ?? 0) + 1} of ${steps}` : "Mathlib setup"}
          />
        </span>
      )}
      <MathlibSetupDialog />
    </>
  );
}

/** How a blocked package is prepared, by state. */
function preparation(inspection: LeanPackageInspection): { title: string; canPrepare: boolean; canBuild: boolean } {
  switch (inspection.readiness.kind) {
    case "cacheMissing":
      return { title: "Mathlib's prebuilt cache is not in this package", canPrepare: true, canBuild: true };
    case "dependenciesMissing":
      return { title: "The pinned dependencies are not downloaded", canPrepare: true, canBuild: false };
    case "dependenciesUnresolved":
      return { title: "The dependencies are not resolved", canPrepare: true, canBuild: false };
    case "unfinished":
      // Building from source stays possible when the setup did not touch the dependency sources.
      return { title: "A Mathlib setup did not finish here", canPrepare: true, canBuild: inspection.readiness.canBuildFromSource };
    case "toolchainUnpinned":
      // Set Up Mathlib explains which lean-toolchain the package needs.
      return { title: "The Lean version is not pinned", canPrepare: true, canBuild: false };
    case "toolchainMismatch":
      return { title: "The Lean version does not match this Mathlib revision", canPrepare: false, canBuild: false };
    default:
      return { title: "Lean cannot check this file", canPrepare: false, canBuild: false };
  }
}

async function buildFromSource(state: LeanDocumentState): Promise<void> {
  const choice = await confirm<"cancel" | "build">({
    title: "Build Mathlib from source?",
    message:
      "Lean builds every Mathlib module this file imports on this computer. For most of Mathlib this takes hours and several GB of memory. The result is the same library as the prebuilt cache; the choice lasts until Exposition closes.",
    actions: [
      { id: "cancel", label: "Cancel", variant: "secondary" },
      { id: "build", label: "Build from source", variant: "primary" },
    ],
    cancelId: "cancel",
    defaultId: "cancel",
    tone: "warning",
  });
  if (choice === "build") await commands.leanAllowLocalBuild(state.document);
}

/**
 * Shown by the infoview when Lean does not check a file because its package
 * is not prepared: the reason from the Lean service word for word, and the
 * supported way to prepare it. Null when the package is not the cause.
 */
export function PackagePreparation({ state }: { state: LeanDocumentState }) {
  const [inspection, setInspection] = useState<LeanPackageInspection | null>(null);
  const setup = useLeanSetup((s) => s.status);
  const reason = state.status.kind === "unavailable" ? state.status.reason : null;
  useEffect(() => startLeanSetupTracking(), []);
  useEffect(() => {
    let live = true;
    if (reason === null) {
      setInspection(null);
      return;
    }
    void commands
      .leanPackageFor(state.document)
      .then((found) => live && setInspection(found))
      .catch(() => live && setInspection(null));
    return () => {
      live = false;
    };
  }, [state.document, reason, setup?.state, setup?.id]);
  if (reason === null) return null;
  const sameFolder = setup !== null && inspection !== null && setup.plan.folder === inspection.folder;
  if (sameFolder && setup?.state === "running") return <MathlibSetupProgress status={setup} />;
  const view = inspection && inspection.usesMathlib ? preparation(inspection) : null;
  if (!inspection || !view || inspection.readiness.kind === "ready") {
    return (
      <Banner tone="warning" title="Lean cannot check this file">
        {reason}
      </Banner>
    );
  }
  return (
    <div
      className="exp-lean-setup__preparation"
      data-testid="lean-package-preparation"
      data-readiness={inspection.readiness.kind}
      data-folder={inspection.folder}
    >
      <Banner
        tone="warning"
        title={view.title}
        actions={
          view.canPrepare || view.canBuild ? (
            <>
              {view.canPrepare && (
                <Button
                  size="sm"
                  variant="primary"
                  icon={PackageCheck}
                  onPress={() => openMathlibSetup({ kind: "prepare", folder: inspection.folder })}
                  data-testid="lean-prepare-open"
                >
                  Set Up Mathlib…
                </Button>
              )}
              {view.canBuild && (
                <Button size="sm" icon={Hammer} onPress={() => void buildFromSource(state)} data-testid="lean-build-from-source">
                  Build from source instead…
                </Button>
              )}
            </>
          ) : undefined
        }
      >
        {reason}
      </Banner>
    </div>
  );
}
