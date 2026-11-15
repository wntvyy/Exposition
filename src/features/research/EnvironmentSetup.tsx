// Guided environment setup: what an unavailable environment needs, the setup
// plan (exactly what will change, before anything runs), progress with
// Cancel, and an honest outcome. Python project environments are set up by
// Exposition (project-local `.venv`, validated packages from PyPI); missing
// runtimes link to their official download pages and are detected again
// automatically; R/Julia kernel packages are commands the person runs.
import { useCallback, useEffect, useRef, useState } from "react";
import { Copy, ExternalLink, PackagePlus, RefreshCw, ShieldAlert, Square, Wrench } from "lucide-react";

import { notify } from "../../app/notifications";
import type { RuntimeEnvironment } from "../../bindings/RuntimeEnvironment";
import type { SetupPlan } from "../../bindings/SetupPlan";
import type { SetupStatus } from "../../bindings/SetupStatus";
import { commands, errorMessage, isCommandError } from "../../lib/ipc";
import {
  Badge,
  Banner,
  Button,
  Dialog,
  IconButton,
  KeyValueList,
  LoadingState,
  ProgressBar,
  Section,
  Select,
  StatusText,
  TextField,
  UnavailableState,
  type KeyValueItem,
} from "../../ui/components";
import {
  commandLine,
  setupChoiceFor,
  setupRequest,
  splitPackages,
  systemPythons,
  type SetupChoice,
} from "./runloop";
import { cancelSetup, dismissSetupOutcome, planSetup, startSetup, useSetup } from "./setup";
import { loadEnvironments, useResearch, userSession } from "./store";

/** Codes of a plan refused for the packages typed (shown on the field). */
const PACKAGE_CODES = ["setup.invalidPackage", "setup.tooManyPackages"];

async function openOfficialPage(url: string): Promise<void> {
  try {
    await commands.appOpenExternal(url);
  } catch (e) {
    notify({ tone: "danger", title: "Could not open the page", body: errorMessage(e) });
  }
}

async function copy(text: string, what: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
    notify({ tone: "info", title: `${what} copied` });
  } catch {
    notify({ tone: "warning", title: "Could not copy", body: text });
  }
}

/** Re-detect environments when the window gets focus again (the person may have installed something). */
function useRedetectOnFocus(active: boolean): void {
  const last = useRef(0);
  useEffect(() => {
    if (!active) return;
    const check = () => {
      if (document.visibilityState !== "visible") return;
      const now = Date.now();
      if (now - last.current < 4000 || useResearch.getState().environmentsLoading) return;
      last.current = now;
      void loadEnvironments(true);
    };
    window.addEventListener("focus", check);
    document.addEventListener("visibilitychange", check);
    return () => {
      window.removeEventListener("focus", check);
      document.removeEventListener("visibilitychange", check);
    };
  }, [active]);
}

// ---- the plan dialog --------------------------------------------------------

export interface SetupDialogProps {
  choice: SetupChoice;
  isOpen: boolean;
  onOpenChange: (open: boolean) => void;
}

type PlanLoad =
  | { kind: "loading" }
  | { kind: "ready"; plan: SetupPlan }
  | { kind: "error"; code: string; message: string };

/** Review exactly what a setup changes, then start it (the person's explicit action). */
export function SetupDialog({ choice, isOpen, onOpenChange }: SetupDialogProps) {
  const environments = useResearch((s) => s.environments);
  const bases = systemPythons(environments);
  const [base, setBase] = useState<string | null>(choice.kind === "createEnvironment" ? choice.base : null);
  const [packagesText, setPackagesText] = useState("");
  const [load, setLoad] = useState<PlanLoad>({ kind: "loading" });
  const [starting, setStarting] = useState(false);
  const [failure, setFailure] = useState<{ code: string; message: string } | null>(null);
  const serial = useRef(0);

  // Reset when the dialog opens or proposes something else (not on every parent render).
  const choiceKey = JSON.stringify(choice);
  useEffect(() => {
    if (!isOpen) return;
    const current = JSON.parse(choiceKey) as SetupChoice;
    setBase(current.kind === "createEnvironment" ? current.base : null);
    setPackagesText("");
    setFailure(null);
  }, [isOpen, choiceKey]);

  const packages = splitPackages(packagesText);
  const request = setupRequest(choice, packages, base);
  const requestKey = JSON.stringify(request);

  const fetchPlan = useCallback(async () => {
    const mine = ++serial.current;
    const current = requestKey === "null" ? null : (JSON.parse(requestKey) as ReturnType<typeof setupRequest>);
    if (!current) {
      setLoad({ kind: "error", code: "setup.noBase", message: "No Python was found on this computer to create the environment from." });
      return;
    }
    setLoad({ kind: "loading" });
    try {
      const plan = await planSetup(current);
      if (mine === serial.current) setLoad({ kind: "ready", plan });
    } catch (e) {
      if (mine === serial.current) setLoad({ kind: "error", code: isCommandError(e) ? e.code : "error", message: errorMessage(e) });
    }
  }, [requestKey]);

  useEffect(() => {
    if (!isOpen) return;
    const timer = setTimeout(() => void fetchPlan(), packagesText ? 350 : 0);
    return () => clearTimeout(timer);
  }, [isOpen, fetchPlan, packagesText]);

  const plan = load.kind === "ready" ? load.plan : null;
  const packageError = load.kind === "error" && PACKAGE_CODES.includes(load.code) ? load.message : null;
  const stale = failure?.code === "setup.planStale";

  const start = async () => {
    if (!plan) return;
    setStarting(true);
    setFailure(null);
    try {
      await startSetup(plan);
      onOpenChange(false);
    } catch (e) {
      setFailure({ code: isCommandError(e) ? e.code : "error", message: errorMessage(e) });
    } finally {
      setStarting(false);
    }
  };

  const create = choice.kind === "createEnvironment";
  const facts: KeyValueItem[] = plan
    ? [
        {
          key: "folder",
          label: "Folder",
          value: (
            <>
              <code className="ui-code">{plan.targetName}</code>{" "}
              {plan.createsEnvironment ? "— new, inside the project" : "— the project's existing environment"}
            </>
          ),
          valueProps: { "data-testid": "setup-folder" },
        },
        ...(plan.baseDisplayName
          ? [
              {
                key: "base",
                label: "Created from",
                value: (
                  <>
                    {plan.baseDisplayName} <code className="ui-code">{plan.baseInterpreter}</code>
                  </>
                ),
              },
            ]
          : []),
        {
          key: "packages",
          label: "Packages",
          value: <code className="ui-code">{plan.packages.join(" ")}</code>,
          valueProps: { "data-testid": "setup-packages" },
        },
        {
          key: "network",
          label: "Downloads",
          value: (
            <>
              From PyPI (<code className="ui-code">{plan.indexUrl}</code>), binary packages only
            </>
          ),
        },
        {
          key: "runs",
          label: "Runs",
          value: (
            <Badge tone="warning" icon={ShieldAlert}>
              On this computer with your account&rsquo;s privileges
            </Badge>
          ),
        },
        {
          key: "recovery",
          label: "If it fails",
          value: plan.createsEnvironment
            ? `The new ${plan.targetName} is removed; nothing else in the project changes.`
            : "Packages already there stay; nothing is removed.",
        },
      ]
    : [];

  return (
    <Dialog
      title={create ? "Set up environment" : "Install packages"}
      size="lg"
      body="content"
      isOpen={isOpen}
      onOpenChange={onOpenChange}
      isDismissable={!starting}
      footer={
        <>
          <Button slot="close" isDisabled={starting}>
            Cancel
          </Button>
          <Button
            variant="primary"
            icon={create ? Wrench : PackagePlus}
            isPending={starting}
            isDisabled={!plan || starting || stale}
            onPress={() => void start()}
            data-testid="setup-start"
          >
            {create ? "Set up" : "Install"}
          </Button>
        </>
      }
    >
      <div className="exp-setup" data-testid="setup-plan" data-state={load.kind} data-plan-hash={plan?.planHash}>
        {create && bases.length > 1 && (
          <Select<string>
            label="Create from"
            options={bases.map((b) => ({ id: b.id, label: `${b.displayName} — ${b.executable ?? ""}` }))}
            value={base}
            onChange={(id) => setBase(id)}
            wrapValue
          />
        )}
        {load.kind === "loading" && !plan && <LoadingState title="Preparing the plan…" fill={false} />}
        {plan && (
          <>
            <p data-testid="setup-summary">{plan.summary}</p>
            <KeyValueList items={facts} />
          </>
        )}
        <div data-testid="setup-extra-packages">
          <TextField
            label="Additional packages"
            description="Separate with spaces, for example numpy pandas>=2.2. Package names and versions only."
            value={packagesText}
            onChange={setPackagesText}
            isInvalid={packageError !== null}
            errorMessage={packageError ?? undefined}
            code
          />
        </div>
        {load.kind === "error" && !packageError && (
          <Banner tone="danger" title={load.code === "setup.noBase" ? "No Python found" : "This setup cannot run"}>
            <span data-testid="setup-plan-error" data-code={load.code}>
              {load.message}
            </span>
          </Banner>
        )}
        {failure && (
          <div data-testid="setup-start-error" data-code={failure.code}>
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
            <pre className="exp-setup__commands" data-testid="setup-commands" tabIndex={0} aria-label="Commands the setup runs">
              {plan.steps.map((step, i) => `${i + 1}. ${step.label}\n   ${commandLine(step.program, step.args)}`).join("\n")}
            </pre>
            <p className="ui-meta">Each runs directly (no shell) in the project folder.</p>
          </Section>
        )}
      </div>
    </Dialog>
  );
}

// ---- progress and outcome ---------------------------------------------------

/** A running setup: step, live output, Cancel. */
export function SetupProgress({ status }: { status: SetupStatus }) {
  const log = useSetup((s) => s.log);
  const viewer = useRef<HTMLPreElement>(null);
  useEffect(() => {
    const el = viewer.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [log]);
  const steps = status.plan.steps.length;
  const step = status.step ?? 0;
  const label =
    status.phase === "cleaningUp"
      ? `Removing the unfinished ${status.plan.targetName}`
      : status.phase === "checking"
        ? "Checking the environment"
        : status.phase === "preparing"
          ? "Preparing"
          : `Step ${step + 1} of ${steps}: ${status.plan.steps[step]?.label ?? ""}`;
  return (
    <div className="exp-setup__progress" data-testid="setup-progress" data-phase={status.phase} data-step={status.step ?? undefined}>
      <Banner
        tone="info"
        title={status.plan.createsEnvironment ? `Setting up ${status.plan.targetName}` : `Installing packages into ${status.plan.targetName}`}
        actions={
          <Button size="sm" icon={Square} onPress={() => void cancelSetup()} data-testid="setup-cancel">
            Cancel setup
          </Button>
        }
      >
        <ProgressBar label={label} />
        {log.length > 0 && (
          <pre ref={viewer} className="exp-setup__log" tabIndex={0} aria-label="Setup output" data-testid="setup-log">
            {log.slice(-12).join("\n")}
          </pre>
        )}
      </Banner>
    </div>
  );
}

/** A finished setup that did not succeed: what happened, and what to do next. */
export function SetupOutcome({
  status,
  onRetry,
  retryLabel = "Set up again…",
}: {
  status: SetupStatus;
  onRetry?: () => void;
  retryLabel?: string;
}) {
  const log = useSetup((s) => s.log);
  const [showLog, setShowLog] = useState(false);
  const canceled = status.state === "canceled";
  return (
    <div data-testid="setup-outcome" data-state={status.state} data-removed={String(status.removed)}>
      <Banner
        tone={canceled ? "canceled" : "danger"}
        title={
          status.plan.createsEnvironment
            ? canceled
              ? "Setup canceled"
              : "Setup failed"
            : canceled
              ? "Installation canceled"
              : "Installation failed"
        }
        actions={
          <>
            {onRetry && (
              <Button size="sm" icon={Wrench} onPress={onRetry}>
                {retryLabel}
              </Button>
            )}
            {log.length > 0 && (
              <Button size="sm" variant="ghost" onPress={() => setShowLog((v) => !v)} aria-expanded={showLog}>
                {showLog ? "Hide output" : "Show output"}
              </Button>
            )}
            <Button size="sm" variant="ghost" onPress={() => dismissSetupOutcome(status.id)}>
              Dismiss
            </Button>
          </>
        }
      >
        <span data-testid="setup-outcome-message">{status.message}</span>
        {showLog && (
          <pre className="exp-setup__log" tabIndex={0} aria-label="Setup output">
            {log.join("\n")}
          </pre>
        )}
      </Banner>
    </div>
  );
}

// ---- what an unavailable environment needs ---------------------------------

/**
 * The selected environment is not usable: the concrete next step. Exposition
 * sets up Python project environments itself; it links to official installers
 * for missing runtimes (and checks again when the window gets focus); R/Julia
 * kernel packages are commands to copy. The reason and guidance come from
 * discovery, word for word.
 */
export function EnvironmentHelp({ env }: { env: RuntimeEnvironment }) {
  const environments = useResearch((s) => s.environments);
  const trusted = useResearch((s) => s.workspace?.trusted ?? false);
  const loading = useResearch((s) => s.environmentsLoading);
  const [open, setOpen] = useState(false);
  const hint = env.setup ?? null;
  const choice = setupChoiceFor(env, environments);
  const commands = hint?.commands ?? [];
  const external = Boolean(hint?.downloadUrl || commands.length > 0);
  useRedetectOnFocus(external);
  if (env.availability.kind !== "unavailable") return null;
  const program = hint?.commandsFor ?? "the runtime";
  const install = choice?.kind === "installPackages";
  const runtimeName = env.language === "python" ? "Python" : env.language === "r" ? "R" : "Julia";
  return (
    <div data-testid="research-unavailable" data-environment={env.id} data-setup={choice?.kind ?? "none"}>
      <UnavailableState
        fill={false}
        title={`${env.displayName} is not available`}
        description={
          <>
            <p>{env.availability.reason}</p>
            <p className="exp-research__guidance" data-testid="setup-guidance">
              {env.availability.guidance}
            </p>
            {choice && !trusted && <p>Trust the folder first: setup runs on this computer with your account&rsquo;s privileges.</p>}
            {hint?.userWide && commands.length > 0 && (
              <p className="ui-meta">The {program} commands install for your user account, not only for this project.</p>
            )}
            {external && <p className="ui-meta">Exposition checks again when you come back to this window.</p>}
          </>
        }
        actions={
          <>
            {choice && (
              <Button size="sm" variant="primary" icon={install ? PackagePlus : Wrench} onPress={() => setOpen(true)} data-testid="setup-open">
                {install ? "Install packages…" : "Set up environment…"}
              </Button>
            )}
            {hint?.downloadUrl && (
              <Button
                size="sm"
                variant={choice ? "secondary" : "primary"}
                icon={ExternalLink}
                onPress={() => void openOfficialPage(hint.downloadUrl as string)}
                data-testid="setup-download"
              >
                {`Get ${runtimeName}`}
              </Button>
            )}
            {commands.length > 0 && (
              <Button size="sm" icon={Copy} onPress={() => void copy(commands.join("\n"), `${program} commands`)} data-testid="setup-copy-commands">
                {`Copy ${program} commands`}
              </Button>
            )}
            <Button size="sm" icon={RefreshCw} isPending={loading} onPress={() => void loadEnvironments(true)}>
              Check again
            </Button>
          </>
        }
      />
      {choice && <SetupDialog choice={choice} isOpen={open} onOpenChange={setOpen} />}
    </div>
  );
}

/** Session bar action: install into the project environment, or create one (never into a system Python). */
export function SetupButton() {
  const environments = useResearch((s) => s.environments);
  const selected = useResearch((s) => s.selectedEnvironment);
  const running = useSetup((s) => s.status?.state === "running");
  const [open, setOpen] = useState(false);
  const env = environments?.runtimes.find((r) => r.id === selected);
  const choice = env?.language === "python" ? setupChoiceFor(env, environments) : null;
  if (!choice) return null;
  const label = choice.kind === "installPackages" ? "Install packages…" : "Set up a project environment…";
  return (
    <>
      <IconButton
        icon={PackagePlus}
        size="sm"
        aria-label={label}
        isDisabled={running}
        onPress={() => setOpen(true)}
        data-testid="setup-packages-open"
      />
      <SetupDialog choice={choice} isOpen={open} onOpenChange={setOpen} />
    </>
  );
}

/** The setup area above the run bar: progress, an unsuccessful outcome, or what the selected environment needs. */
export function SetupArea() {
  const status = useSetup((s) => s.status);
  const dismissed = useSetup((s) => s.dismissed);
  const environments = useResearch((s) => s.environments);
  const selected = useResearch((s) => s.selectedEnvironment);
  const session = useResearch((s) => userSession(s));
  const [retry, setRetry] = useState(false);
  const env = environments?.runtimes.find((r) => r.id === selected);
  if (status?.state === "running") return <SetupProgress status={status} />;
  const outcome =
    status && (status.state === "failed" || status.state === "canceled") && !dismissed.includes(status.id) ? status : null;
  const retryChoice = outcome ? setupChoiceFor(env, environments) : null;
  return (
    <>
      {outcome && (
        <>
          <SetupOutcome
            status={outcome}
            onRetry={retryChoice ? () => setRetry(true) : undefined}
            retryLabel={retryChoice?.kind === "installPackages" ? "Install packages…" : "Set up again…"}
          />
          {retryChoice && <SetupDialog choice={retryChoice} isOpen={retry} onOpenChange={setRetry} />}
        </>
      )}
      {!session && env && env.availability.kind === "unavailable" && <EnvironmentHelp env={env} />}
      {status?.state === "succeeded" && !dismissed.includes(status.id) && env?.id === status.environmentId && (
        <p className="ui-meta" data-testid="setup-succeeded">
          <StatusText status="succeeded" label={`${status.plan.targetName} is ready`} />
        </p>
      )}
    </>
  );
}
