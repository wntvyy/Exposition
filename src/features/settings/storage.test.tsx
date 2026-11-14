// Settings › Storage & History: categories and sizes from storage_overview,
// referenced/shared items kept by default (never in "Select unreferenced"),
// the selected scope previewed honestly, purge through plan/apply, excluded
// backups disclosed.
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { StorageOverview } from "../../bindings/StorageOverview";

const opIpc = vi.hoisted(() => ({ plan: vi.fn(), apply: vi.fn(), restore: vi.fn(), storageOverview: vi.fn() }));
vi.mock("../operations/ipc", () => ({ opCommands: opIpc, opEvents: { projectOp: vi.fn(async () => () => {}) } }));
const notifications = vi.hoisted(() => ({ notify: vi.fn(), dismiss: vi.fn() }));
vi.mock("../../app/notifications", () => notifications);
// The real workspace store pulls in Monaco; this view only reads the open folder.
vi.mock("../workspace/store", async () => {
  const { create } = await import("zustand");
  return { useWorkspace: create<{ info: { root: string; name: string } | null }>(() => ({ info: null })) };
});

import { ConfirmHost } from "../../ui/components";
import { useWorkspace } from "../workspace/store";
import { purgeGroups, StorageSection, storageScope } from "./StorageSection";

const overview: StorageOverview = {
  categories: [
    {
      id: "workflowRuns",
      label: "Workflow runs",
      bytes: 3 * 1024 * 1024,
      note: "Deleting removes the record only; outputs stay unless you include them.",
      items: [
        { target: { kind: "workflowRun", run: "wrun-1" }, label: "Fit model — 12 Nov", bytes: 2 * 1024 * 1024, createdAtMs: 0, referenced: false, active: false },
        { target: { kind: "workflowRun", run: "wrun-2" }, label: "Fit model — 13 Nov", bytes: null, createdAtMs: 0, referenced: false, active: true },
      ],
    },
    {
      id: "workflowRevisions",
      label: "Workflow revisions",
      bytes: null,
      note: "Revisions other workflows pin are kept unless you include them.",
      items: [{ target: { kind: "workflowRevision", workflow: "wf-1", revision: 1 }, label: "Fit model, revision 1 (pinned by Pipeline)", bytes: null, createdAtMs: 0, referenced: true, active: false }],
    },
  ],
  excluded: ["Graph backups in .exposition/graph-backups (kept by the graph service)"],
};

beforeEach(() => {
  for (const fn of Object.values(opIpc)) fn.mockReset();
  notifications.notify.mockReset();
  opIpc.storageOverview.mockResolvedValue(overview);
  useWorkspace.setState({ info: { root: "C:/proj", name: "proj" } as never });
});
afterEach(() => {
  cleanup();
  useWorkspace.setState({ info: null });
});

describe("storage scope", () => {
  it("sums measurable sizes and counts what is not measured, referenced, or active", () => {
    const scope = storageScope(overview, new Set(["workflowRuns|workflowRun:wrun-1", "workflowRuns|workflowRun:wrun-2", "workflowRevisions|workflowRevision:wf-1@1"]));
    expect(scope).toMatchObject({ bytes: 2 * 1024 * 1024, unmeasured: 2, referenced: 1, active: 1 });
  });

  it("purges records, records with their outputs, and outputs alone as separate plans (the outputs choice is per plan)", () => {
    const run = (id: string) => ({ kind: "workflowRun" as const, run: id });
    const item = (id: string) => ({ target: run(id), label: id, bytes: null, createdAtMs: null, referenced: false, active: false });
    expect(
      purgeGroups([
        { category: "workflowRuns", item: item("a") },
        { category: "workflowRuns", item: item("b") },
        { category: "generatedOutputs", item: item("b") },
        { category: "generatedOutputs", item: item("c") },
      ]),
    ).toEqual([
      { targets: [run("a")] },
      { targets: [run("b")], outputs: "delete" },
      { targets: [run("c")], outputs: "only" },
    ]);
  });
});

describe("Storage & History", () => {
  it("keeps referenced items out of the default selection, previews the scope, and purges through the plan", async () => {
    render(
      <>
        <ConfirmHost />
        <StorageSection />
      </>,
    );
    const runs = await screen.findByTestId("storage-category-workflowRuns");
    expect(runs.textContent).toContain("Workflow runs (2 · 3.0 MiB)");
    expect(runs.textContent).toContain("outputs stay");
    expect(screen.getByTestId("storage-excluded").textContent).toContain("Graph backups");

    // "Select unreferenced" in revisions offers nothing: the only revision is pinned.
    const revisions = screen.getByTestId("storage-category-workflowRevisions");
    expect(within(revisions).queryByRole("button", { name: /Select unreferenced/ })).toBeNull();
    expect(revisions.textContent).toContain("Referenced");

    fireEvent.click(within(runs).getByRole("button", { name: "Select unreferenced" }));
    const scope = screen.getByTestId("storage-scope");
    await waitFor(() => expect(scope.getAttribute("data-count")).toBe("2"));
    expect(scope.textContent).toContain("2 selected · 2.0 MiB of files (+1 not measured)");
    expect(scope.textContent).toContain("frees database space only when the database is compacted");
    expect(scope.textContent).toContain("1 active item is included");

    opIpc.plan.mockImplementation(async (request) => ({
      plan: "p",
      request,
      effects: request.targets.map((target: unknown) => ({ target, description: "Delete the record", destination: null, recovery: { kind: "none" } })),
      refusals: [],
      impacts: [],
      needsDecision: false,
      choices: [],
      planHash: "h",
      expiresAtMs: 0,
    }));
    opIpc.apply.mockResolvedValue({ plan: "p", outcomes: [], notices: ["Removed 2 run records; 2.0 MiB of output files deleted. Database space is reclaimed at the next compaction."] });
    fireEvent.click(screen.getByTestId("storage-purge"));
    await waitFor(() => expect(opIpc.apply).toHaveBeenCalled());
    expect(opIpc.plan).toHaveBeenCalledWith({
      kind: { kind: "delete", permanent: true },
      targets: [{ kind: "workflowRun", run: "wrun-1" }, { kind: "workflowRun", run: "wrun-2" }],
      actor: { kind: "user" },
    });
  });

  it("including a referenced item says it will ask first", async () => {
    render(<StorageSection />);
    const revisions = await screen.findByTestId("storage-category-workflowRevisions");
    fireEvent.click(within(revisions).getByRole("checkbox"));
    await waitFor(() => expect(screen.getByTestId("storage-scope").textContent).toContain("1 referenced item is included: you will see what depends on it first."));
  });
});
