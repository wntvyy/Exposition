// The run loop and setup choices as pure functions.
import { describe, expect, it } from "vitest";

import type { EnvironmentReport } from "../../bindings/EnvironmentReport";
import type { RunSummary } from "../../bindings/RunSummary";
import type { RuntimeEnvironment } from "../../bindings/RuntimeEnvironment";
import type { SessionInfo } from "../../bindings/SessionInfo";
import {
  activeUserRunId,
  commandLine,
  lastUserRun,
  pickEnvironment,
  projectPython,
  rerunDescription,
  setupChoiceFor,
  setupRequest,
  splitPackages,
  systemPythons,
} from "./runloop";

const run = (id: string, startedAtMs: number, overrides: Partial<RunSummary> = {}): RunSummary => ({
  id,
  sessionId: "s1",
  owner: { kind: "user" },
  status: { kind: "succeeded" },
  queued: false,
  origin: { kind: "input" },
  codePreview: "print(1)",
  executionCount: 1,
  startedAtMs,
  finishedAtMs: startedAtMs + 1,
  outputs: [],
  error: null,
  ...overrides,
});

const env = (id: string, overrides: Partial<RuntimeEnvironment> = {}): RuntimeEnvironment => ({
  id,
  language: "python",
  displayName: id,
  executable: `C:\\${id}\\python.exe`,
  version: "3.12.10",
  kernel: { kind: "python" },
  hostInterpreter: null,
  packages: [],
  kernelEnvironment: {},
  availability: { kind: "available" },
  ...overrides,
});

const unavailable = { kind: "unavailable" as const, reason: "lacks ipykernel.", guidance: "…" };
const report = (...runtimes: RuntimeEnvironment[]): EnvironmentReport => ({ runtimes, scannedAtMs: 0 });

describe("run loop", () => {
  it("reruns the newest run the user started, never an agent's", () => {
    const runs = [
      run("a", 10),
      run("agent", 30, { owner: { kind: "agent", taskId: "t" } }),
      run("b", 20, { origin: { kind: "file", path: "research/analysis.py", revision: 3 } }),
    ];
    expect(lastUserRun(runs)?.id).toBe("b");
    expect(rerunDescription(lastUserRun(runs))).toBe("Rerun research/analysis.py");
    expect(lastUserRun([run("agent", 1, { owner: { kind: "agent", taskId: "t" } })])).toBeUndefined();
    expect(rerunDescription(undefined)).toBe("Rerun: nothing has run yet");
    expect(rerunDescription(run("i", 1))).toBe("Rerun the Research input");
    expect(
      rerunDescription(run("s", 1, { origin: { kind: "selection", path: "a.py", revision: 2, range: { start: 0, end: 3 } } })),
    ).toBe("Rerun the selection in a.py");
  });

  it("stops the run the user's session executes now", () => {
    const session = { activeRun: "run-7" } as SessionInfo;
    expect(activeUserRunId(session, [])).toBe("run-7");
    const runs = [run("q", 2, { status: { kind: "running" }, queued: true }), run("r", 1, { status: { kind: "running" } })];
    expect(activeUserRunId(undefined, runs)).toBe("r");
    expect(activeUserRunId({ activeRun: null } as SessionInfo, [run("done", 1)])).toBeNull();
  });

  it("selects the session choice, else the remembered one, else the first usable", () => {
    const r = report(env("python:a", { availability: unavailable }), env("python:b"), env("python:c"));
    expect(pickEnvironment(r, "python:c", "python:a")).toBe("python:c");
    expect(pickEnvironment(r, null, "python:a")).toBe("python:a");
    // A stale remembered id falls back to the first usable environment.
    expect(pickEnvironment(r, "python:gone", "python:gone-too")).toBe("python:b");
    expect(pickEnvironment(report(env("x", { availability: unavailable })), null, null)).toBe("x");
    expect(pickEnvironment(report(), null, null)).toBeNull();
  });
});

describe("setup choices", () => {
  const system312 = env("python:312", { version: "3.12.10", availability: unavailable, setup: { downloadUrl: null, action: "createEnvironment", commands: [], commandsFor: null, userWide: false } });
  const system314 = env("python:314", { version: "3.14.0" });
  const venv = env("python:venv", { projectFolder: ".venv" });
  const venvLacking = env("python:venv", {
    projectFolder: ".venv",
    availability: unavailable,
    setup: { downloadUrl: null, action: "installPackages", commands: [], commandsFor: null, userWide: false },
  });
  const unfinished = env("python:venv", {
    projectFolder: ".venv",
    availability: unavailable,
    setup: { downloadUrl: null, action: "createEnvironment", commands: [], commandsFor: null, userWide: false },
  });
  const rNoHost = env("r:none", {
    language: "r",
    availability: unavailable,
    setup: { downloadUrl: null, action: "createEnvironment", commands: [], commandsFor: null, userWide: false },
  });
  const rKernel = env("r:none", {
    language: "r",
    availability: unavailable,
    setup: { downloadUrl: null, action: null, commands: ["IRkernel::installspec()"], commandsFor: "R", userWide: true },
  });
  const noPython = env("python:none", {
    executable: null,
    availability: unavailable,
    setup: { downloadUrl: "https://www.python.org/downloads/", action: null, commands: [], commandsFor: null, userWide: false },
  });

  it("orders system Pythons newest first and finds the project environment", () => {
    const r = report(system312, system314, venv);
    expect(systemPythons(r).map((e) => e.id)).toEqual(["python:314", "python:312"]);
    expect(projectPython(r)?.id).toBe("python:venv");
  });

  it("creates the project environment from a system Python, or installs into the existing one", () => {
    expect(setupChoiceFor(system312, report(system312, system314))).toEqual({ kind: "createEnvironment", base: "python:312" });
    expect(setupChoiceFor(system312, report(system312, venv))).toEqual({ kind: "installPackages", environment: "python:venv" });
    expect(setupChoiceFor(venv, report(system312, venv))).toEqual({ kind: "installPackages", environment: "python:venv" });
    expect(setupChoiceFor(venvLacking, report(system312, venvLacking))).toEqual({ kind: "installPackages", environment: "python:venv" });
    // An unfinished environment is set up again from the newest system Python.
    expect(setupChoiceFor(unfinished, report(system312, system314, unfinished))).toEqual({ kind: "createEnvironment", base: "python:314" });
    // A system Python never receives packages: an unfinished project environment does not count.
    expect(setupChoiceFor(system312, report(system312, unfinished))).toEqual({ kind: "createEnvironment", base: "python:312" });
  });

  it("offers no setup where Exposition cannot do it", () => {
    expect(setupChoiceFor(noPython, report(noPython))).toBeNull();
    expect(setupChoiceFor(rKernel, report(rKernel, system314))).toBeNull();
    expect(setupChoiceFor(undefined, null)).toBeNull();
    // R without a Python host: a project Python environment hosts its kernel.
    expect(setupChoiceFor(rNoHost, report(rNoHost, system312))).toEqual({ kind: "createEnvironment", base: "python:312" });
    expect(setupChoiceFor(rNoHost, report(rNoHost, venv))).toEqual({ kind: "installPackages", environment: "python:venv" });
  });

  it("builds the request Rust validates", () => {
    expect(setupRequest({ kind: "createEnvironment", base: "python:312" }, ["numpy"])).toEqual({
      kind: "createEnvironment",
      baseEnvironment: "python:312",
      packages: ["numpy"],
    });
    expect(setupRequest({ kind: "createEnvironment", base: "python:312" }, [], "python:314")).toEqual({
      kind: "createEnvironment",
      baseEnvironment: "python:314",
      packages: [],
    });
    expect(setupRequest({ kind: "createEnvironment", base: null }, [])).toBeNull();
    expect(setupRequest({ kind: "installPackages", environment: "python:venv" }, ["scipy"])).toEqual({
      kind: "installPackages",
      environment: "python:venv",
      packages: ["scipy"],
    });
  });

  it("splits typed packages without breaking version clauses or extras", () => {
    expect(splitPackages("numpy pandas>=2.2")).toEqual(["numpy", "pandas>=2.2"]);
    expect(splitPackages("numpy, pandas\nscipy")).toEqual(["numpy", "pandas", "scipy"]);
    expect(splitPackages("scipy >= 1.11, < 2")).toEqual(["scipy>=1.11,<2"]);
    expect(splitPackages("matplotlib[qt, io]==3.9.* numpy")).toEqual(["matplotlib[qt,io]==3.9.*", "numpy"]);
    expect(splitPackages("   ")).toEqual([]);
    // Hostile text is passed on unchanged for Rust to refuse (never "cleaned" into something else).
    expect(splitPackages("-r requirements.txt")).toEqual(["-r", "requirements.txt"]);
  });

  it("shows commands with arguments that need quoting quoted", () => {
    expect(commandLine("C:\\Python312\\python.exe", ["-I", "-m", "venv", "C:\\My Project\\.venv"])).toBe(
      'C:\\Python312\\python.exe -I -m venv "C:\\My Project\\.venv"',
    );
    expect(commandLine("py", ["pandas>=2.2", "--only-binary=:all:"])).toBe('py "pandas>=2.2" --only-binary=:all:');
  });
});
