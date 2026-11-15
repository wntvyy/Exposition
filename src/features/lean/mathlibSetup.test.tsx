// Guided Mathlib setup in the Lean UI: the plan shown before anything runs
// (package folder, pinned Lean, Mathlib revision, files created and kept,
// every command), refusals from Rust on the field they concern, progress
// with Cancel, an honest outcome, and what the infoview offers for a package
// that is not prepared. IPC is mocked at the feature boundary.
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { LeanDocumentState } from "../../bindings/LeanDocumentState";
import type { LeanPackageInspection } from "../../bindings/LeanPackageInspection";
import type { LeanSetupEvent } from "../../bindings/LeanSetupEvent";
import type { LeanSetupPlan } from "../../bindings/LeanSetupPlan";
import type { LeanSetupStatus } from "../../bindings/LeanSetupStatus";

vi.hoisted(() => {
  globalThis.matchMedia ??= ((query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addEventListener() {},
    removeEventListener() {},
    addListener() {},
    removeListener() {},
    dispatchEvent: () => false,
  })) as unknown as typeof matchMedia;
});

const ipc = vi.hoisted(() => ({
  leanProjects: vi.fn(async () => [] as unknown[]),
  leanPackageFor: vi.fn(),
  leanSetupPlan: vi.fn(),
  leanSetupStart: vi.fn(),
  leanSetupCancel: vi.fn(async () => true),
  leanSetupStatus: vi.fn(async () => null),
  leanAllowLocalBuild: vi.fn(async () => undefined),
  trustStatus: vi.fn(async () => ({ trusted: true, root: "C:\\p" })),
}));
const handlers = vi.hoisted(() => [] as ((e: LeanSetupEvent) => void)[]);
const notify = vi.hoisted(() => vi.fn());

vi.mock("../../lib/ipc", () => ({
  commands: ipc,
  events: {
    leanSetup: vi.fn(async (handler: (e: LeanSetupEvent) => void) => {
      handlers.push(handler);
      return () => {};
    }),
    leanSetupResync: vi.fn(async () => () => {}),
  },
  errorMessage: (e: unknown) => (e && typeof e === "object" && "message" in e ? String((e as { message: string }).message) : String(e)),
  isCommandError: (e: unknown) => Boolean(e && typeof e === "object" && "code" in e),
}));
vi.mock("../../app/notifications", () => ({ notify }));
vi.mock("../jobs/trust", () => ({ requestWorkspaceTrust: vi.fn(async () => true) }));

const { MathlibSetupDialog, MathlibSetupStatusItem, PackagePreparation } = await import("./MathlibSetup");
const { applyLeanSetupEvent, openMathlibSetup, resetLeanSetupForTests, useLeanSetup } = await import("./setup");

const LAKE = "C:\\Users\\u\\.elan\\toolchains\\leanprover--lean4---v4.35.0\\bin\\lake.exe";

function newPlan(overrides: Partial<LeanSetupPlan> = {}): LeanSetupPlan {
  return {
    request: { kind: "newMathlibPackage", folder: "lean", name: "Formal" },
    root: "C:\\p",
    packageRoot: "C:\\p\\lean",
    folder: "lean",
    createsPackage: true,
    toolchain: "leanprover/lean4:v4.35.0",
    toolchainInstalled: true,
    mathlibRevision: "v4.35.0",
    pinnedByManifest: false,
    writes: ["lakefile.toml", "lean-toolchain", "Formal.lean", "Formal/Basic.lean", ".gitignore"],
    keeps: [],
    removedOnFailure: ["lakefile.toml", ".lake", ""],
    steps: [
      { label: "Resolve the dependencies and write lake-manifest.json", program: LAKE, args: ["--keep-toolchain", "update"], environment: { MATHLIB_NO_CACHE_ON_UPDATE: "1" }, network: true, timeoutMs: 1 },
      { label: "Download Mathlib's prebuilt cache for the pinned revision", program: LAKE, args: ["exe", "cache", "get"], environment: {}, network: true, timeoutMs: 1 },
      { label: "Build the starter library Formal with the cache", program: LAKE, args: ["build", "Formal"], environment: {}, network: false, timeoutMs: 1 },
    ],
    summary: "Creates the Lake package Formal in `lean`.",
    planHash: "h1",
    ...overrides,
  };
}

function preparePlan(): LeanSetupPlan {
  return newPlan({
    request: { kind: "prepareMathlibPackage", folder: "lean" },
    createsPackage: false,
    mathlibRevision: "56a005855ea17e0b0958f73ec801ae140b0a1679",
    pinnedByManifest: true,
    writes: [],
    keeps: ["lakefile.toml", "lean-toolchain", "lake-manifest.json"],
    removedOnFailure: [".lake"],
    steps: [
      { label: "Download Mathlib's prebuilt cache for the pinned revision", program: LAKE, args: ["exe", "cache", "get"], environment: {}, network: true, timeoutMs: 1 },
      { label: "Verify that Mathlib's build outputs are complete (builds nothing)", program: LAKE, args: ["build", "--no-build", "Mathlib"], environment: {}, network: false, timeoutMs: 1 },
    ],
    planHash: "h2",
  });
}

function status(plan: LeanSetupPlan, overrides: Partial<LeanSetupStatus> = {}): LeanSetupStatus {
  return {
    id: "s1",
    plan,
    state: "running",
    phase: "step",
    step: 1,
    message: null,
    removed: [],
    inspection: null,
    ready: false,
    jobs: [],
    log: [],
    startedAtMs: 1,
    finishedAtMs: null,
    ...overrides,
  };
}

function inspection(readiness: LeanPackageInspection["readiness"], overrides: Partial<LeanPackageInspection> = {}): LeanPackageInspection {
  return {
    folder: "lean",
    root: "C:\\p\\lean",
    lakefile: "lakefile.toml",
    toolchain: "leanprover/lean4:v4.35.0",
    toolchainInstalled: true,
    toolchainProblem: null,
    manifestDigest: "ab".repeat(32),
    manifestProblem: null,
    dependencies: [],
    usesMathlib: true,
    mathlibRevision: "56a005855ea17e0b0958f73ec801ae140b0a1679",
    mathlibInputRevision: "v4.35.0",
    mathlibToolchain: null,
    cachePresent: false,
    readiness,
    ...overrides,
  };
}

const lakeProject = { path: "lean", root: "C:\\p\\lean", kind: { kind: "lake", lakefile: "lakefile.toml" }, toolchain: { kind: "invalid", reason: "" }, server: { kind: "stopped" } };

function unavailable(reason: string): LeanDocumentState {
  return { document: "d1", path: "lean/Formal/Basic.lean", revision: 1, status: { kind: "unavailable", reason } } as unknown as LeanDocumentState;
}

describe("Mathlib setup", () => {
  beforeEach(() => {
    resetLeanSetupForTests();
    for (const mock of Object.values(ipc)) mock.mockClear();
    ipc.leanProjects.mockResolvedValue([]);
    ipc.leanSetupStatus.mockResolvedValue(null);
    notify.mockReset();
  });
  afterEach(() => cleanup());

  it("shows the whole plan of a new package before anything runs, then starts exactly that plan", async () => {
    const plan = newPlan();
    ipc.leanSetupPlan.mockResolvedValue(plan);
    ipc.leanSetupStart.mockResolvedValue(status(plan, { step: 0 }));
    render(<MathlibSetupDialog />);
    act(() => openMathlibSetup({ kind: "new" }));
    await waitFor(() => expect(screen.getByTestId("lean-setup-plan").getAttribute("data-state")).toBe("ready"));
    expect(ipc.leanSetupPlan).toHaveBeenLastCalledWith({ kind: "newMathlibPackage", folder: "lean", name: "Formal" });
    const summary = screen.getByTestId("lean-setup-summary").textContent ?? "";
    expect(summary).toContain("C:\\p\\lean");
    expect(summary).toContain("a new Lake package");
    expect(summary).toContain("leanprover/lean4:v4.35.0");
    expect(summary).toContain("default toolchain is not changed");
    expect(summary).toContain("the release made for this Lean version");
    expect(summary).toContain("Formal/Basic.lean");
    expect(summary).toContain("several GB");
    expect(summary).toContain("On this computer with your account’s privileges");
    expect(summary).toContain("The new package's files are removed");
    // Every command, exactly as launched, with the variable it sets and which ones download.
    const commands = screen.getByTestId("lean-setup-commands").textContent ?? "";
    expect(commands).toContain(`MATHLIB_NO_CACHE_ON_UPDATE=1 ${LAKE} --keep-toolchain update`);
    expect(commands).toContain(`2. Download Mathlib's prebuilt cache for the pinned revision (downloads)\n   ${LAKE} exe cache get`);
    expect(commands).toContain(`3. Build the starter library Formal with the cache\n   ${LAKE} build Formal`);
    expect(ipc.leanSetupStart).not.toHaveBeenCalled();
    fireEvent.click(screen.getByTestId("lean-setup-start"));
    await waitFor(() => expect(ipc.leanSetupStart).toHaveBeenCalledWith(plan));
    // The dialog now shows the running setup with Cancel.
    await waitFor(() => expect(screen.getByTestId("lean-setup-progress")).toBeTruthy());
    expect(screen.getByTestId("lean-setup-progress").textContent).toContain("Step 1 of 3: Resolve the dependencies");
    fireEvent.click(screen.getByTestId("lean-setup-cancel"));
    await waitFor(() => expect(ipc.leanSetupCancel).toHaveBeenCalledWith("s1"));
  });

  it("puts a refusal on the field it concerns and offers no start", async () => {
    ipc.leanSetupPlan.mockRejectedValue({ code: "lean.setup.insidePackage", message: "`lean/sub` is inside the Lake package in `lean` (lakefile.toml)." });
    render(<MathlibSetupDialog />);
    act(() => openMathlibSetup({ kind: "new" }));
    await waitFor(() => expect(screen.getByTestId("lean-setup-plan").getAttribute("data-state")).toBe("error"));
    expect(screen.getByTestId("lean-setup-folder-field").textContent).toContain("is inside the Lake package");
    expect(screen.queryByTestId("lean-setup-plan-error")).toBeNull();
    expect(screen.getByTestId("lean-setup-start").hasAttribute("disabled")).toBe(true);
  });

  it("prepares an existing package from its manifest and says nothing is upgraded", async () => {
    ipc.leanProjects.mockResolvedValue([lakeProject]);
    ipc.leanSetupPlan.mockResolvedValue(preparePlan());
    render(<MathlibSetupDialog />);
    act(() => openMathlibSetup({ kind: "prepare", folder: "lean" }));
    await waitFor(() => expect(screen.getByTestId("lean-setup-plan").getAttribute("data-state")).toBe("ready"));
    expect(ipc.leanSetupPlan).toHaveBeenLastCalledWith({ kind: "prepareMathlibPackage", folder: "lean" });
    const summary = screen.getByTestId("lean-setup-summary").textContent ?? "";
    expect(summary).toContain("the revision lake-manifest.json pins; nothing is upgraded");
    expect(summary).toContain("Keeps unchanged");
    expect(summary).toContain("lake-manifest.json");
    expect(summary).not.toContain("Creates");
    expect(screen.getByTestId("lean-setup-commands").textContent).not.toContain("update");
    expect(screen.getByTestId("lean-setup-start").textContent).toContain("Prepare package");
  });

  it("says a prepared package needs nothing and a changed project needs a new plan", async () => {
    ipc.leanProjects.mockResolvedValue([lakeProject]);
    ipc.leanSetupPlan.mockRejectedValueOnce({ code: "lean.setup.alreadyPrepared", message: "The package in `lean` is already prepared. Nothing needs to be downloaded." });
    render(<MathlibSetupDialog />);
    act(() => openMathlibSetup({ kind: "prepare", folder: "lean" }));
    await waitFor(() => expect(screen.getByTestId("lean-setup-plan-error").getAttribute("data-code")).toBe("lean.setup.alreadyPrepared"));
    expect(screen.getByTestId("lean-setup-start").hasAttribute("disabled")).toBe(true);
    cleanup();
    resetLeanSetupForTests();

    ipc.leanSetupPlan.mockResolvedValue(preparePlan());
    ipc.leanSetupStart.mockRejectedValue({ code: "lean.setup.planStale", message: "The project changed since this plan was made." });
    render(<MathlibSetupDialog />);
    act(() => openMathlibSetup({ kind: "prepare", folder: "lean" }));
    await waitFor(() => expect(screen.getByTestId("lean-setup-plan").getAttribute("data-state")).toBe("ready"));
    fireEvent.click(screen.getByTestId("lean-setup-start"));
    await waitFor(() => expect(screen.getByTestId("lean-setup-start-error").getAttribute("data-code")).toBe("lean.setup.planStale"));
    expect(screen.getByTestId("lean-setup-start-error").textContent).toContain("Nothing was changed.");
    expect(screen.getByRole("button", { name: "Review the new plan" })).toBeTruthy();
  });

  it("follows progress output and reports a failed setup with what happened to the package", async () => {
    const plan = newPlan();
    render(
      <>
        <MathlibSetupStatusItem />
      </>,
    );
    act(() => openMathlibSetup({ kind: "auto" }));
    act(() => applyLeanSetupEvent({ kind: "changed", status: status(plan) }));
    expect(screen.getByTestId("lean-setup-status-item").textContent).toContain("Mathlib setup: step 2 of 3");
    act(() =>
      applyLeanSetupEvent({ kind: "output", setupId: "s1", step: 1, text: "Downloaded: 1 file(s) [attempted 1/9031 = 0%]\rDownloaded: 70 file(s)\n\u001b[31merror:\u001b[0m no route\n" }),
    );
    expect(useLeanSetup.getState().log).toEqual(["Downloaded: 70 file(s)", "error: no route"]);
    expect(screen.getByTestId("lean-setup-log").textContent).toContain("error: no route");
    const message = "Download Mathlib's prebuilt cache for the pinned revision failed (exit code 1): error: no route. The new package's files were removed; nothing else in the project changed.";
    act(() =>
      applyLeanSetupEvent({
        kind: "changed",
        status: status(plan, { state: "failed", phase: "finished", message, removed: ["lakefile.toml", ".lake", ""], log: ["error: no route"] }),
      }),
    );
    expect(screen.queryByTestId("lean-setup-status-item")).toBeNull();
    const outcome = screen.getByTestId("lean-setup-outcome");
    expect(outcome.getAttribute("data-state")).toBe("failed");
    expect(screen.getByTestId("lean-setup-outcome-message").textContent).toBe(message);
    expect(screen.getByTestId("lean-setup-retry")).toBeTruthy();
    // The dialog shows the outcome, so no separate notification repeats it.
    expect(notify).not.toHaveBeenCalled();
  });

  it("notifies when a setup ends while its dialog is hidden", () => {
    const plan = preparePlan();
    act(() => applyLeanSetupEvent({ kind: "changed", status: status(plan) }));
    act(() => applyLeanSetupEvent({ kind: "changed", status: status(plan, { state: "succeeded", phase: "finished", ready: true }) }));
    expect(notify).toHaveBeenCalledTimes(1);
    expect(notify.mock.calls[0]![0].title).toBe("Mathlib is ready");
    act(() => applyLeanSetupEvent({ kind: "changed", status: status(plan, { state: "succeeded", phase: "finished", ready: true }) }));
    expect(notify).toHaveBeenCalledTimes(1);
  });

  it("offers the supported preparation in the infoview, and a source build only when just the cache is missing", async () => {
    const reason = "Mathlib's prebuilt cache is not in this package, so Lean would build Mathlib from source.";
    ipc.leanPackageFor.mockResolvedValue(inspection({ kind: "cacheMissing" }));
    render(<PackagePreparation state={unavailable(reason)} />);
    await waitFor(() => expect(screen.getByTestId("lean-package-preparation").getAttribute("data-readiness")).toBe("cacheMissing"));
    expect(screen.getByTestId("lean-package-preparation").textContent).toContain(reason);
    expect(screen.getByTestId("lean-build-from-source")).toBeTruthy();
    fireEvent.click(screen.getByTestId("lean-prepare-open"));
    expect(useLeanSetup.getState().dialog).toEqual({ kind: "prepare", folder: "lean" });
    cleanup();

    ipc.leanPackageFor.mockResolvedValue(inspection({ kind: "dependenciesMissing", names: ["mathlib"] }));
    render(<PackagePreparation state={unavailable("The dependencies this package pins are not downloaded (mathlib).")} />);
    await waitFor(() => expect(screen.getByTestId("lean-package-preparation").getAttribute("data-readiness")).toBe("dependenciesMissing"));
    expect(screen.queryByTestId("lean-build-from-source")).toBeNull();
    expect(screen.getByTestId("lean-prepare-open")).toBeTruthy();
    cleanup();

    // A mismatch is the user's configuration decision: explained, with nothing to run.
    ipc.leanPackageFor.mockResolvedValue(inspection({ kind: "toolchainMismatch", package: "a", mathlib: "b" }));
    render(<PackagePreparation state={unavailable("This package pins Lean a, but its Mathlib revision is built for b.")} />);
    await waitFor(() => expect(screen.getByTestId("lean-package-preparation").getAttribute("data-readiness")).toBe("toolchainMismatch"));
    expect(screen.queryByTestId("lean-prepare-open")).toBeNull();
    cleanup();

    // Another cause (no package, or a package without Mathlib): the reason alone, as before.
    ipc.leanPackageFor.mockResolvedValue(null);
    render(<PackagePreparation state={unavailable("Open the folder that contains this file to check it with Lean.")} />);
    await waitFor(() => expect(ipc.leanPackageFor).toHaveBeenCalled());
    expect(screen.getByText("Lean cannot check this file")).toBeTruthy();
    expect(screen.queryByTestId("lean-package-preparation")).toBeNull();
  });
});
