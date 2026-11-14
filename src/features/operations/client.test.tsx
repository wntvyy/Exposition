// The shared operation client against a mocked backend contract: routine
// operations apply at once with no dialog; a plan that needs a decision shows
// ONE combined decision naming the consumers (Cancel changes nothing); actual
// partial success/failure is reported; Undo only with restore tokens; a
// restore never overwrites (collision → explicit choice); a stale plan is
// re-planned and decided again, never applied blindly.
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OpOutcome } from "../../bindings/OpOutcome";
import type { OpPlan } from "../../bindings/OpPlan";
import type { OpRequest } from "../../bindings/OpRequest";
import type { OpResult } from "../../bindings/OpResult";

const ipc = vi.hoisted(() => ({
  plan: vi.fn(),
  apply: vi.fn(),
  restore: vi.fn(),
  clipboardFilesRead: vi.fn(),
  clipboardFilesWrite: vi.fn(),
  storageOverview: vi.fn(),
}));
vi.mock("./ipc", () => ({ opCommands: ipc, opEvents: { projectOp: vi.fn(async () => () => {}) } }));
const notifications = vi.hoisted(() => ({ notify: vi.fn(), dismiss: vi.fn() }));
vi.mock("../../app/notifications", () => notifications);

import { ConfirmHost } from "../../ui/components";
import { onProjectOp, opHistory, operationInFlight, redoLast, runOperation, undoLast } from "./client";

const del = (paths: string[], permanent = false): OpRequest => ({
  kind: { kind: "delete", permanent },
  targets: paths.map((path) => ({ kind: "path", path })),
  actor: { kind: "user" },
});

function plan(request: OpRequest, patch: Partial<OpPlan> = {}): OpPlan {
  return {
    plan: "plan-1",
    request,
    effects: request.targets.map((target) => ({ target, description: "Move to the Recycle Bin", destination: null, recovery: { kind: "osTrash" } })),
    refusals: [],
    impacts: [],
    needsDecision: false,
    choices: [],
    planHash: "hash-1",
    expiresAtMs: Date.now() + 60_000,
    ...patch,
  };
}

function outcome(path: string, patch: Partial<OpOutcome> = {}): OpOutcome {
  return { target: { kind: "path", path }, status: "succeeded", code: null, message: null, created: null, recovery: { kind: "osTrash" }, restore: null, ...patch };
}

const result = (outcomes: OpOutcome[], notices: string[] = []): OpResult => ({ plan: "plan-1", outcomes, notices });

function lastNotification() {
  const calls = notifications.notify.mock.calls;
  return calls[calls.length - 1]?.[0];
}

beforeEach(() => {
  for (const fn of Object.values(ipc)) fn.mockReset();
  notifications.notify.mockReset();
  opHistory.clear();
  render(<ConfirmHost />);
});

afterEach(() => cleanup());

describe("routine operations", () => {
  it("delete an unused file: no dialog, applied at once, truthful recovery, Undo with the restore token", async () => {
    const request = del(["notes/old.md"]);
    ipc.plan.mockResolvedValue(plan(request));
    ipc.apply.mockResolvedValue(result([outcome("notes/old.md", { restore: "tok-1" })]));
    const seen: OpResult[] = [];
    const off = onProjectOp((r) => seen.push(r));

    const done = await runOperation(request);

    expect(done?.result.outcomes[0]?.status).toBe("succeeded");
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(ipc.apply).toHaveBeenCalledWith({ plan: "plan-1", planHash: "hash-1", choices: {} });
    const note = lastNotification();
    expect(note.tone).toBe("info");
    expect(note.title).toBe("Moved “old.md” to the Recycle Bin");
    expect(note.actions.map((a: { label: string }) => a.label)).toEqual(["Undo"]);
    expect(seen).toHaveLength(1);
    expect(opHistory.canUndo()).toBe(true);
    off();
  });

  it("without a restore token there is no Undo, and the recovery is described honestly", async () => {
    const request = del(["a.txt", "b.txt"]);
    ipc.plan.mockResolvedValue(plan(request));
    ipc.apply.mockResolvedValue(result([outcome("a.txt"), outcome("b.txt")]));
    await runOperation(request);
    const note = lastNotification();
    expect(note.title).toBe("Moved 2 items to the Recycle Bin");
    expect(note.body).toContain("Restore them from the Recycle Bin");
    expect(note.actions).toBeUndefined();
    expect(opHistory.canUndo()).toBe(false);
  });

  it("partial failure is reported per item and stays until dismissed", async () => {
    const request = del(["a.txt", "locked.txt"]);
    ipc.plan.mockResolvedValue(plan(request));
    ipc.apply.mockResolvedValue(
      result([outcome("a.txt"), outcome("locked.txt", { status: "failed", code: "ops.inUse", message: "locked.txt is open in another program; close it there and try again." })]),
    );
    await runOperation(request);
    const note = lastNotification();
    expect(note.tone).toBe("warning");
    expect(note.title).toBe("Moved 1 of 2 items to the Recycle Bin");
    expect(note.body).toContain("“locked.txt”: locked.txt is open in another program; close it there and try again.");
    // The backend already says what to do; no second hint, and no Delete Permanently for a file in use.
    expect(note.body).not.toContain("Close the program that is using it");
    expect(note.actions).toBeUndefined();
    expect(note.timeoutMs).toBeNull();
  });

  it("trash unavailable: no silent permanent fallback; Delete Permanently is offered as a new, explicit plan", async () => {
    const request = del(["a.txt"]);
    ipc.plan.mockResolvedValueOnce(plan(request));
    ipc.apply.mockResolvedValueOnce(result([outcome("a.txt", { status: "failed", code: "ops.trashUnavailable", message: "The Recycle Bin is unavailable for this drive." })]));
    await runOperation(request);
    const note = lastNotification();
    expect(note.tone).toBe("danger");
    expect(note.title).toBe("Could not delete “a.txt”");
    expect(ipc.apply).toHaveBeenCalledTimes(1);
    const action = note.actions.find((a: { label: string }) => a.label === "Delete Permanently…");
    expect(action).toBeTruthy();

    const permanent = del(["a.txt"], true);
    ipc.plan.mockResolvedValueOnce(
      plan(permanent, {
        needsDecision: true,
        impacts: [{ kind: "recoveryLoss", target: { kind: "path", path: "a.txt" }, consumers: [], message: "“a.txt” cannot be restored after this." }],
      }),
    );
    void action.run();
    await waitFor(() => expect(ipc.plan).toHaveBeenLastCalledWith(permanent));
    const dialog = await screen.findByRole("alertdialog");
    expect(within(dialog).getByText("“a.txt” cannot be restored after this.")).toBeTruthy();
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
    expect(ipc.apply).toHaveBeenCalledTimes(1);
  });

  it("everything refused: nothing is applied and the reasons are shown", async () => {
    const request = del(["../outside.txt"]);
    ipc.plan.mockResolvedValue(
      plan(request, { effects: [], refusals: [{ target: request.targets[0]!, code: "ops.outsideProject", message: "It is outside the project folder." }] }),
    );
    expect(await runOperation(request)).toBeNull();
    expect(ipc.apply).not.toHaveBeenCalled();
    const note = lastNotification();
    expect(note.title).toBe("Nothing to delete");
    expect(note.body).toContain("It is outside the project folder.");
  });

  it("a saved-version notice is non-modal", async () => {
    const request: OpRequest = { kind: { kind: "duplicate" }, targets: [{ kind: "path", path: "main.typ" }], actor: { kind: "user" } };
    ipc.plan.mockResolvedValue(
      plan(request, {
        impacts: [{ kind: "savedVersionCopied", target: request.targets[0]!, consumers: [], message: "main.typ has unsaved changes; its saved version was copied." }],
      }),
    );
    ipc.apply.mockResolvedValue(result([outcome("main.typ", { created: { kind: "path", path: "main (2).typ" }, recovery: { kind: "none" } })]));
    await runOperation(request);
    expect(screen.queryByRole("alertdialog")).toBeNull();
    const titles = notifications.notify.mock.calls.map((c) => c[0].title);
    expect(titles).toContain("Duplicated “main.typ” as “main (2).typ”");
    expect(titles).toContain("Saved version copied");
  });

  it("a refusal with guidance offers the follow-up action (e.g. Copy Text for a source-anchored item)", async () => {
    const request: OpRequest = { kind: { kind: "duplicate" }, targets: [{ kind: "graphEntity", entity: "ent-thm" }], actor: { kind: "user" } };
    ipc.plan.mockResolvedValue(
      plan(request, { effects: [], refusals: [{ target: request.targets[0]!, code: "ops.sourceAnchored", message: "Theorem 2 is anchored in its source; use Copy Reference or Copy Text instead." }] }),
    );
    const copyText = vi.fn();
    await runOperation(request, { onRefused: (refused) => (refused.some((r) => r.code === "ops.sourceAnchored") ? [{ label: "Copy Text", run: copyText }] : []) });
    const note = lastNotification();
    expect(note.title).toBe("Nothing to duplicate");
    expect(note.body).toContain("anchored in its source");
    expect(note.actions.map((a: { label: string }) => a.label)).toEqual(["Copy Text"]);
    expect(ipc.apply).not.toHaveBeenCalled();
  });

  it("a second invocation while one is in flight is ignored (held key, double click)", async () => {
    const request = del(["a.txt"]);
    let release!: (p: OpPlan) => void;
    ipc.plan.mockReturnValue(new Promise<OpPlan>((r) => (release = r)));
    ipc.apply.mockResolvedValue(result([outcome("a.txt")]));
    const first = runOperation(request);
    expect(operationInFlight()).toBe(true);
    expect(await runOperation(request)).toBeNull();
    release(plan(request));
    await first;
    expect(ipc.plan).toHaveBeenCalledTimes(1);
    expect(ipc.apply).toHaveBeenCalledTimes(1);
  });
});

describe("the combined decision", () => {
  const request = del(["figures/plot.png", "data/raw.csv"]);
  const decisionPlan = () =>
    plan(request, {
      needsDecision: true,
      impacts: [
        {
          kind: "manuscriptDependency",
          target: request.targets[0]!,
          consumers: [{ kind: "document", id: "d1", label: "paper.typ", path: "paper.typ" }],
          message: "paper.typ includes this figure; it will show as missing.",
        },
        {
          kind: "activeWork",
          target: request.targets[1]!,
          consumers: [{ kind: "run", id: "wrun-1", label: "Fit model (running)", path: null }],
          message: "A running workflow reads this file.",
        },
      ],
      choices: [{ key: "activeWork", label: "Running work", options: [{ value: "keep", label: "Cancel deletion of this file" }, { value: "stop", label: "Stop the run, then delete" }], default: "keep" }],
    });

  it("names consumers once for the whole batch; Cancel is focused and changes nothing", async () => {
    ipc.plan.mockResolvedValue(decisionPlan());
    const pending = runOperation(request);
    const dialog = await screen.findByRole("alertdialog");
    expect(within(dialog).getByText("Delete 2 items")).toBeTruthy();
    expect(within(dialog).getByText("paper.typ includes this figure; it will show as missing.")).toBeTruthy();
    expect(within(dialog).getByText("Fit model (running)")).toBeTruthy();
    expect(within(dialog).queryByText(/are you sure/i)).toBeNull();
    expect(document.activeElement).toBe(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(screen.getAllByRole("alertdialog")).toHaveLength(1);
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(await pending).toBeNull();
    expect(ipc.apply).not.toHaveBeenCalled();
  });

  it("Proceed applies once with the chosen values (defaults are never destructive)", async () => {
    ipc.plan.mockResolvedValue(decisionPlan());
    ipc.apply.mockResolvedValue(result([outcome("figures/plot.png"), outcome("data/raw.csv")]));
    const pending = runOperation(request);
    const dialog = await screen.findByRole("alertdialog");
    expect(dialog.querySelector('[data-choice="activeWork"]')?.getAttribute("data-value")).toBe("keep");
    fireEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
    await pending;
    expect(ipc.apply).toHaveBeenCalledTimes(1);
    expect(ipc.apply).toHaveBeenCalledWith({ plan: "plan-1", planHash: "hash-1", choices: { activeWork: "keep" } });
  });

  it("an explicitly scoped action presets its choice and is always reviewed", async () => {
    const runDelete: OpRequest = { kind: { kind: "delete", permanent: false }, targets: [{ kind: "workflowRun", run: "wrun-1" }], actor: { kind: "user" } };
    ipc.plan.mockResolvedValue(
      plan(runDelete, {
        choices: [{ key: "outputs", label: "Outputs", options: [{ value: "keep", label: "Keep outputs" }, { value: "delete", label: "Delete outputs" }], default: "keep" }],
      }),
    );
    ipc.apply.mockResolvedValue(result([{ ...outcome("x"), target: runDelete.targets[0]!, recovery: { kind: "appManaged" } }]));
    const pending = runOperation(runDelete, { presetChoices: { outputs: "delete" }, review: true });
    const dialog = await screen.findByRole("alertdialog");
    expect(dialog.querySelector('[data-choice="outputs"]')?.getAttribute("data-value")).toBe("delete");
    fireEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
    await pending;
    expect(ipc.apply).toHaveBeenCalledWith(expect.objectContaining({ choices: { outputs: "delete" } }));
  });

  it("a preset scope the plan does not offer is stated, not implied", async () => {
    const runDelete: OpRequest = { kind: { kind: "delete", permanent: false }, targets: [{ kind: "workflowRun", run: "wrun-1" }], actor: { kind: "user" } };
    ipc.plan.mockResolvedValue(plan(runDelete));
    ipc.apply.mockResolvedValue(result([{ ...outcome("x"), target: runDelete.targets[0]!, recovery: { kind: "appManaged" } }]));
    const pending = runOperation(runDelete, { presetChoices: { outputs: "delete" }, review: true });
    const dialog = await screen.findByRole("alertdialog");
    expect(within(dialog).getByText("Deleting the outputs is not offered for this item")).toBeTruthy();
    fireEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
    await pending;
    expect(ipc.apply).toHaveBeenCalledWith(expect.objectContaining({ choices: {} }));
  });

  it("a stale plan is re-planned and decided again, never applied blindly", async () => {
    ipc.plan.mockResolvedValueOnce(plan(request)).mockResolvedValueOnce(decisionPlan());
    ipc.apply.mockRejectedValueOnce({ code: "ops.planStale", message: "The plan changed." }).mockResolvedValueOnce(result([outcome("figures/plot.png"), outcome("data/raw.csv")]));
    const pending = runOperation(request);
    const dialog = await screen.findByRole("alertdialog");
    expect(within(dialog).getByText("The items or their consequences changed since this was planned.")).toBeTruthy();
    fireEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
    await pending;
    expect(ipc.plan).toHaveBeenCalledTimes(2);
    expect(ipc.apply).toHaveBeenCalledTimes(2);
  });

  it("the person's choice survives a re-plan (stop stays stop)", async () => {
    ipc.plan.mockResolvedValueOnce(decisionPlan()).mockResolvedValueOnce(decisionPlan());
    ipc.apply
      .mockRejectedValueOnce({ code: "ops.planStale", message: "The plan changed." })
      .mockResolvedValueOnce(result([outcome("figures/plot.png"), outcome("data/raw.csv")]));
    const pending = runOperation(request);
    let dialog = await screen.findByRole("alertdialog");
    fireEvent.click(within(dialog).getByRole("button", { name: /Running work/ }));
    fireEvent.click(await screen.findByRole("option", { name: "Stop the run, then delete" }));
    await waitFor(() => expect(dialog.querySelector('[data-choice="activeWork"]')?.getAttribute("data-value")).toBe("stop"));
    fireEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(screen.getByText("The items or their consequences changed since this was planned.")).toBeTruthy());
    dialog = screen.getByRole("alertdialog");
    expect(dialog.querySelector('[data-choice="activeWork"]')?.getAttribute("data-value")).toBe("stop");
    fireEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
    await pending;
    expect(ipc.apply).toHaveBeenLastCalledWith({ plan: "plan-1", planHash: "hash-1", choices: { activeWork: "stop" } });
  });
});

describe("undo, redo and restore", () => {
  it("Undo restores; an occupied original location asks before using a new name (both kept)", async () => {
    const request = del(["a.txt"]);
    ipc.plan.mockResolvedValue(plan(request));
    ipc.apply.mockResolvedValue(result([outcome("a.txt", { restore: "tok-a" })]));
    await runOperation(request);

    ipc.restore
      .mockResolvedValueOnce({ ...outcome("a.txt"), status: "skipped", code: "ops.restoreCollision", message: "a.txt exists." })
      .mockResolvedValueOnce({ ...outcome("a.txt"), created: { kind: "path", path: "a (2).txt" } });
    const undo = undoLast();
    const dialog = await screen.findByRole("alertdialog");
    expect(within(dialog).getByText("“a.txt” already exists")).toBeTruthy();
    fireEvent.click(within(dialog).getByRole("button", { name: "Restore with a New Name" }));
    await act(async () => {
      await undo;
    });
    expect(ipc.restore).toHaveBeenNthCalledWith(1, { restore: "tok-a", onCollision: "cancel" });
    expect(ipc.restore).toHaveBeenNthCalledWith(2, { restore: "tok-a", onCollision: "rename" });
    expect(lastNotification().title).toBe("Restored “a.txt” as “a (2).txt”");
    expect(opHistory.canRedo()).toBe(true);

    // Redo deletes the restored copy again, through a new plan.
    ipc.plan.mockResolvedValue(plan(del(["a (2).txt"])));
    ipc.apply.mockResolvedValue(result([outcome("a (2).txt")]));
    await redoLast();
    expect(ipc.plan).toHaveBeenLastCalledWith(del(["a (2).txt"]));
  });

  it("the project's Undo list survives a restart (tokens and names only); another project has its own", async () => {
    localStorage.clear();
    opHistory.setScope("C:/proj");
    const request = del(["notes.md"]);
    ipc.plan.mockResolvedValue(plan(request));
    ipc.apply.mockResolvedValue(result([outcome("notes.md", { restore: "trash-1" })]));
    await runOperation(request);
    // Restart: the in-memory history is gone; opening the same project brings it back.
    opHistory.clear();
    expect(opHistory.canUndo()).toBe(false);
    opHistory.setScope("C:/other");
    expect(opHistory.canUndo()).toBe(false);
    opHistory.setScope("C:/proj");
    expect(opHistory.canUndo()).toBe(true);
    expect(localStorage.getItem("exposition.operations.undo:C:/proj")).not.toMatch(/content|text/);
    ipc.restore.mockResolvedValueOnce({ ...outcome("notes.md"), code: "ops.restored" });
    await act(async () => {
      await undoLast();
    });
    expect(ipc.restore).toHaveBeenCalledWith({ restore: "trash-1", onCollision: "cancel" });
    expect(lastNotification().title).toBe("Restored “notes.md”");
    opHistory.setScope(null);
    opHistory.setScope("C:/proj");
    expect(opHistory.canUndo()).toBe(false);
  });

  it("Redo while another operation is in flight keeps its entry (nothing is lost)", async () => {
    const request = del(["a.txt"]);
    ipc.plan.mockResolvedValue(plan(request));
    ipc.apply.mockResolvedValue(result([outcome("a.txt", { restore: "tok-a" })]));
    await runOperation(request);
    ipc.restore.mockResolvedValueOnce({ ...outcome("a.txt"), code: "ops.restored" });
    await act(async () => {
      await undoLast();
    });
    expect(opHistory.canRedo()).toBe(true);
    // Another operation is planning (held open).
    let release!: (p: OpPlan) => void;
    ipc.plan.mockReturnValueOnce(new Promise<OpPlan>((r) => (release = r)));
    const busy = runOperation(del(["b.txt"]));
    expect(await redoLast()).toBe(false);
    expect(opHistory.canRedo()).toBe(true);
    ipc.apply.mockResolvedValueOnce(result([outcome("b.txt")]));
    release(plan(del(["b.txt"])));
    await busy;
    expect(opHistory.canRedo()).toBe(true);
  });

  it("Redo cancelled at its decision keeps the entry", async () => {
    const request = del(["a.txt"]);
    ipc.plan.mockResolvedValue(plan(request));
    ipc.apply.mockResolvedValue(result([outcome("a.txt", { restore: "tok-a" })]));
    await runOperation(request);
    ipc.restore.mockResolvedValueOnce({ ...outcome("a.txt"), code: "ops.restored" });
    await act(async () => {
      await undoLast();
    });
    ipc.plan.mockResolvedValueOnce(
      plan(request, { needsDecision: true, impacts: [{ kind: "manuscriptDependency", target: request.targets[0]!, consumers: [], message: "paper.typ uses a.txt." }] }),
    );
    const redo = redoLast();
    const dialog = await screen.findByRole("alertdialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(await redo).toBe(false);
    expect(opHistory.canRedo()).toBe(true);
  });

  it("the notification's Undo waits its turn: nothing is restored while another operation is in flight", async () => {
    const request = del(["a.txt"]);
    ipc.plan.mockResolvedValue(plan(request));
    ipc.apply.mockResolvedValue(result([outcome("a.txt", { restore: "tok-a" })]));
    await runOperation(request);
    const undo = lastNotification().actions.find((a: { label: string }) => a.label === "Undo");
    let release!: (p: OpPlan) => void;
    ipc.plan.mockReturnValueOnce(new Promise<OpPlan>((r) => (release = r)));
    const busy = runOperation(del(["b.txt"]));
    await undo.run();
    expect(ipc.restore).not.toHaveBeenCalled();
    expect(opHistory.canUndo()).toBe(true);
    expect(lastNotification().title).toBe("Another operation is still running");
    ipc.apply.mockResolvedValueOnce(result([outcome("b.txt")]));
    release(plan(del(["b.txt"])));
    await busy;
  });

  it("leaving it deleted restores nothing", async () => {
    const request = del(["a.txt"]);
    ipc.plan.mockResolvedValue(plan(request));
    ipc.apply.mockResolvedValue(result([outcome("a.txt", { restore: "tok-a" })]));
    await runOperation(request);
    ipc.restore.mockResolvedValueOnce({ ...outcome("a.txt"), status: "skipped", code: "ops.restoreCollision", message: "a.txt exists." });
    const undo = undoLast();
    const dialog = await screen.findByRole("alertdialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Leave Deleted" }));
    await act(async () => {
      await undo;
    });
    expect(ipc.restore).toHaveBeenCalledTimes(1);
    expect(lastNotification().title).toBe("Could not restore “a.txt”");
  });

  it("only the backend's collision code asks about a new name; other refusals are reported as they are", async () => {
    const request = del(["a.txt"]);
    ipc.plan.mockResolvedValue(plan(request));
    ipc.apply.mockResolvedValue(result([outcome("a.txt", { restore: "trash-1" })]));
    await runOperation(request);
    ipc.restore.mockResolvedValueOnce({ ...outcome("a.txt"), status: "failed", code: "ops.occupied", message: "Something unexpected." });
    await act(async () => {
      await undoLast();
    });
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(ipc.restore).toHaveBeenCalledTimes(1);
    expect(lastNotification().title).toBe("Could not restore “a.txt”");
    expect(lastNotification().body).toContain("Something unexpected.");
  });
});
