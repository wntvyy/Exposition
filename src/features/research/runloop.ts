// The ordinary research loop as pure functions: which run Rerun repeats,
// which run Stop cancels, which environment to select, and what setup an
// environment offers. Unit-tested; the store and views call these.
import type { EnvironmentReport } from "../../bindings/EnvironmentReport";
import type { RunAuthorization } from "../../bindings/RunAuthorization";
import type { RunSummary } from "../../bindings/RunSummary";
import type { RuntimeEnvironment } from "../../bindings/RuntimeEnvironment";
import type { SessionInfo } from "../../bindings/SessionInfo";
import type { SetupRequest } from "../../bindings/SetupRequest";

/** The newest run the user started (Rerun repeats its kind of input). */
export function lastUserRun(runs: RunSummary[]): RunSummary | undefined {
  let newest: RunSummary | undefined;
  for (const run of runs) {
    if (run.owner.kind !== "user") continue;
    if (!newest || run.startedAtMs > newest.startedAtMs) newest = run;
  }
  return newest;
}

/** The run the user's session executes now (Stop cancels it). */
export function activeUserRunId(session: SessionInfo | undefined, runs: RunSummary[]): string | null {
  if (session?.activeRun) return session.activeRun;
  const running = runs.find((r) => r.owner.kind === "user" && r.status.kind === "running" && !r.queued);
  return running?.id ?? null;
}

/** The authorization an agent's run recorded, as one quiet line ("Agent task t-1 · Auto: project permissions v3"). */
export function authorizationText(authorization: RunAuthorization): string {
  const mode = authorization.mode === "auto" ? "Auto" : "Ask";
  const basis =
    authorization.basis === "approved"
      ? "approved by you"
      : authorization.basis === "taskAllowance"
        ? "allowed for this task"
        : `project permissions v${authorization.permissionsVersion ?? "?"}`;
  return `Agent task ${authorization.taskId} · ${mode}: ${basis}`;
}

/** What Rerun runs, in words (its accessible name and tooltip). */
export function rerunDescription(run: RunSummary | undefined): string {
  if (!run) return "Rerun: nothing has run yet";
  switch (run.origin.kind) {
    case "input":
      return "Rerun the Research input";
    case "file":
      return `Rerun ${run.origin.path}`;
    case "selection":
      return run.origin.path ? `Rerun the selection in ${run.origin.path}` : "Rerun the selection";
  }
}

/**
 * The environment to select after discovery: the person's choice in this
 * session if still discovered, else the one remembered for the project, else
 * the first usable one, else the first listed.
 */
export function pickEnvironment(report: EnvironmentReport, current: string | null, remembered: string | null): string | null {
  const ids = new Set(report.runtimes.map((r) => r.id));
  if (current && ids.has(current)) return current;
  if (remembered && ids.has(remembered)) return remembered;
  const usable = report.runtimes.find((r) => r.availability.kind === "available");
  return usable?.id ?? report.runtimes[0]?.id ?? null;
}

/** Discovered Pythons installed for the computer (bases for a project environment), newest first. */
export function systemPythons(report: EnvironmentReport | null): RuntimeEnvironment[] {
  if (!report) return [];
  const versionKey = (v: string | null) => (v ?? "0").split(".").map((p) => Number.parseInt(p, 10) || 0);
  const newer = (a: RuntimeEnvironment, b: RuntimeEnvironment) => {
    const x = versionKey(a.version);
    const y = versionKey(b.version);
    for (let i = 0; i < Math.max(x.length, y.length); i += 1) {
      const d = (y[i] ?? 0) - (x[i] ?? 0);
      if (d !== 0) return d;
    }
    return 0;
  };
  return report.runtimes
    .filter((r) => r.language === "python" && r.executable !== null && !r.projectFolder)
    .sort(newer);
}

/** The project environment discovery found (`.venv` first), if any. */
export function projectPython(report: EnvironmentReport | null): RuntimeEnvironment | undefined {
  const envs = (report?.runtimes ?? []).filter((r) => r.language === "python" && r.projectFolder);
  return envs.find((r) => r.projectFolder === ".venv") ?? envs[0];
}

/** What the setup dialog proposes for an environment. */
export type SetupChoice =
  | { kind: "createEnvironment"; base: string | null }
  | { kind: "installPackages"; environment: string };

/**
 * The setup an environment offers. A project environment gets packages
 * installed (an unfinished one is set up again: Rust removes it first when it
 * has a record). A system Python is the base of a new project `.venv`, unless
 * the project already has an environment (then packages go there). R/Julia
 * without a Python to host their kernels get the same project environment.
 * Python itself missing: nothing to set up here (download instead).
 */
export function setupChoiceFor(env: RuntimeEnvironment | undefined, report: EnvironmentReport | null): SetupChoice | null {
  if (!env) return null;
  const project = projectPython(report);
  const usableProject = project && project.setup?.action !== "createEnvironment" ? project : undefined;
  const newestBase = systemPythons(report)[0]?.id ?? null;
  if (env.language === "python") {
    if (env.projectFolder) {
      return env.setup?.action === "createEnvironment"
        ? { kind: "createEnvironment", base: newestBase }
        : { kind: "installPackages", environment: env.id };
    }
    if (!env.executable) return null;
    if (usableProject) return { kind: "installPackages", environment: usableProject.id };
    return { kind: "createEnvironment", base: env.id };
  }
  if (env.setup?.action !== "createEnvironment") return null;
  if (usableProject) return { kind: "installPackages", environment: usableProject.id };
  return { kind: "createEnvironment", base: newestBase };
}

/**
 * Packages typed by the person, separated by spaces, new lines, or commas
 * between names. Commas inside version clauses (scipy>=1,<2) and extras
 * ([qt,io]) stay with their requirement; Rust validates every entry.
 */
export function splitPackages(text: string): string[] {
  const compact = text.replace(/\s*(===|==|!=|<=|>=|~=|<|>|,|\[|\])\s*/g, "$1");
  const out: string[] = [];
  let current = "";
  let depth = 0;
  for (let i = 0; i < compact.length; i += 1) {
    const c = compact[i] as string;
    if (c === "[") depth += 1;
    if (c === "]") depth = Math.max(0, depth - 1);
    const next = compact[i + 1] ?? "";
    const separator = /\s/.test(c) || (c === "," && depth === 0 && !/[<>=!~0-9]/.test(next));
    if (separator) {
      if (current) out.push(current);
      current = "";
      continue;
    }
    current += c;
  }
  if (current) out.push(current);
  return out;
}

export function setupRequest(choice: SetupChoice, packages: string[], base?: string | null): SetupRequest | null {
  if (choice.kind === "installPackages") return { kind: "installPackages", environment: choice.environment, packages };
  const chosen = base ?? choice.base;
  return chosen ? { kind: "createEnvironment", baseEnvironment: chosen, packages } : null;
}

/** A shell-like rendering of one command for display (never executed). */
export function commandLine(program: string, args: string[]): string {
  const quote = (s: string) => (s === "" || /[\s"'<>|&;^%()]/.test(s) ? `"${s.replace(/"/g, '\\"')}"` : s);
  return [quote(program), ...args.map(quote)].join(" ");
}
