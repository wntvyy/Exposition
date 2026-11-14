// The Graph page's composition: PageHeader with the section's toolbar row
// (F6 regions graph-header, graph-toolbar, graph-main, graph-inspector), the
// inspector's pane header, item header, and sections, and the quiet status
// treatments in the inspector and the Verification table.
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../workspace/store", () => ({
  useWorkspace: (select: (s: { info: unknown }) => unknown) => select({ info: { root: "C:/p", name: "p", recovery: [] } }),
}));
vi.mock("./reveal", () => ({ revealSource: vi.fn() }));
vi.mock("../lean/evidence", () => ({ leanEvidenceCommands: { associate: vi.fn() } }));
const opIpc = vi.hoisted(() => ({ plan: vi.fn(), apply: vi.fn(), restore: vi.fn() }));
vi.mock("../operations/ipc", () => ({ opCommands: opIpc, opEvents: { projectOp: vi.fn(async () => () => {}) } }));

const { useLayout } = await import("../../app/layout");
const { ipcGraphApi, setGraphApi } = await import("./api");
const { createFakeGraph, entity, relation } = await import("./fakeApi");
const { GraphPage, mathViewStore, projectViewStore } = await import("./GraphPage");
const { mathBadges } = await import("./Mathematics");
const { VERDICT_VIEW } = await import("./model");
const { useGraphPage } = await import("./navigation");
const { resetGraphForTests } = await import("./store");
type MathRow = import("../../bindings/MathRow").MathRow;

beforeAll(() => {
  globalThis.ResizeObserver ??= class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
  (globalThis as { DOMMatrixReadOnly?: unknown }).DOMMatrixReadOnly ??= class {
    m22 = 1;
  };
  globalThis.matchMedia ??= ((query: string) => ({
    matches: false,
    media: query,
    addEventListener() {},
    removeEventListener() {},
  })) as unknown as typeof matchMedia;
});

const thm = entity("thm", "theorem", "Main theorem", { key: "thm:main" });
const lem = entity("lem", "lemma", "Key lemma");
const ref = entity("ref", "reference", "Rudin 1976");

function row(e: typeof thm, patch: Partial<MathRow>): MathRow {
  return {
    entity: e,
    binding: null,
    informal: "notApplicable",
    formalAssociation: "none",
    formalDeclarations: [],
    verdict: null,
    freshness: null,
    formalDependencies: null,
    evidence: null,
    imported: false,
    ...patch,
  };
}

beforeEach(() => {
  localStorage.clear();
  localStorage.setItem("exposition.graph.mode.project", "table");
  useLayout.setState({ viewportWidth: 1440 });
  useGraphPage.setState({ section: "project" });
  const fake = createFakeGraph({
    entities: [thm, lem, ref],
    relations: [relation("d1", "dependsOn", "thm", "lem", "suggested"), relation("x1", "references", "thm", "ref")],
    mathRows: [
      row(thm, { informal: "noProof", verdict: "notVerified", freshness: { kind: "unknown", reason: "No dependency fingerprints" } }),
      row(lem, { informal: "proofLinked", verdict: "checked", freshness: { kind: "current" } }),
    ],
  });
  setGraphApi(fake.api);
});

afterEach(async () => {
  cleanup();
  await projectViewStore.flush();
  await mathViewStore.flush();
  resetGraphForTests();
  setGraphApi(ipcGraphApi);
});

const region = (name: string) => document.querySelector<HTMLElement>(`[data-region="${name}"]`);

async function selectRow(name: string) {
  const grid = await screen.findByRole("grid", { name: "Entities" });
  const target = (await within(grid).findByRole("rowheader", { name })).closest('[role="row"]') as HTMLElement;
  act(() => target.focus());
  fireEvent.keyDown(target, { key: " ", code: "Space" });
  fireEvent.keyUp(target, { key: " ", code: "Space" });
  return screen.findByTestId("graph-entity-inspector");
}

describe("Graph page", () => {
  it("uses one page header whose toolbar row holds the shown section's tools", async () => {
    render(<GraphPage />);
    const header = region("graph-header")!;
    expect(within(header).getByRole("heading", { name: "Graph" })).toBeTruthy();
    // The headline state is the page's one status pill, with the graph's counts.
    await waitFor(() => expect(within(header).getByText("Graph synchronized").closest(".ui-status-pill")).not.toBeNull());
    expect(within(header).getByTestId("graph-counts").textContent).toMatch(/^3 items · 2 relationships · revision \d+$/);

    await waitFor(() => expect(region("graph-toolbar")).not.toBeNull());
    const toolbarRow = region("graph-toolbar")!;
    const tools = await within(toolbarRow).findByRole("toolbar", { name: "Project graph tools" });
    expect(within(tools).getByRole("searchbox", { name: "Search the graph" })).toBeTruthy();
    expect(region("graph-main")).not.toBeNull();
    expect(region("graph-inspector")).not.toBeNull();

    // Switching sections keeps the header mounted and swaps the toolbar row's tools.
    fireEvent.click(within(header).getByRole("radio", { name: "Mathematics" }));
    await within(toolbarRow).findByRole("toolbar", { name: "Mathematics tools" });
    expect(region("graph-header")).toBe(header);
    expect(within(toolbarRow).queryByRole("toolbar", { name: "Project graph tools" })).toBeNull();
  });

  it("frames the inspector with a pane header, an item header, and sections with counts", async () => {
    render(<GraphPage />);
    const inspectorRegion = await waitFor(() => {
      const found = region("graph-inspector");
      if (!found) throw new Error("no inspector");
      return found;
    });
    const paneTitle = within(inspectorRegion).getByRole("heading", { name: "Inspector" });
    expect(paneTitle.classList.contains("ui-chrome-label")).toBe(true);
    expect(within(inspectorRegion).getByRole("button", { name: "Close inspector" })).toBeTruthy();
    // Nothing selected: a compact state inside the inspector body.
    expect(inspectorRegion.querySelector('.ui-inspector-body .ui-state[data-size="compact"]')).not.toBeNull();

    const inspector = await selectRow("Main theorem");
    const header = inspector.querySelector(".ui-inspector-header")!;
    // The type in its category color (the shared category text role).
    const type = header.querySelector('.ui-category-text[data-category="math"]')!;
    expect(type.textContent).toBe("Theorem");
    expect(within(inspector).getByRole("heading", { name: "Main theorem", level: 3 })).toBeTruthy();
    const actions = within(header as HTMLElement).getByRole("toolbar", { name: "Item actions" });
    expect(within(actions).getAllByRole("button").map((b) => b.getAttribute("aria-label") ?? b.textContent)).toEqual([
      "Open source",
      "Relate to…",
      "Show neighborhood",
      "More actions",
    ]);
    expect(await within(inspector).findByRole("heading", { name: "Relationships 2" })).toBeTruthy();
    expect(within(inspector).getByRole("heading", { name: "Source 1" })).toBeTruthy();
    const binding = within(inspector).getByTestId("graph-binding");
    expect(binding.querySelector(".ui-code")!.textContent).toBe("thm:main");
    expect(binding.textContent).toMatch(/· whole file · resolved/);
    expect(binding.querySelector(".ui-badge")).toBeNull();

    // Two label levels: sentence-case group labels under the section, never accent.
    const group = within(inspector).getByRole("heading", { name: "Mathematical dependencies", level: 4 });
    expect(group.classList.contains("ui-group-label")).toBe(true);
    expect(within(inspector).getByRole("heading", { name: "References", level: 4 })).toBeTruthy();

    // Provisional status is a dashed badge with Accept/Reject; a settled one is a muted suffix.
    const suggested = inspector.querySelector('[data-testid="graph-relation"][data-status="suggested"]') as HTMLElement;
    const badge = within(suggested).getByText("Suggested").closest(".ui-badge")!;
    expect(badge.hasAttribute("data-dashed")).toBe(true);
    expect(within(suggested).getByRole("button", { name: "Accept suggestion" })).toBeTruthy();
    expect(within(suggested).getByRole("button", { name: "Reject suggestion" })).toBeTruthy();
    expect(within(suggested).getByRole("button", { name: "Delete relationship" })).toBeTruthy();
    const accepted = inspector.querySelector('[data-testid="graph-relation"][data-status="accepted"]') as HTMLElement;
    expect(within(accepted).getByText("· accepted").closest(".ui-badge")).toBeNull();
    expect(accepted.textContent).toMatch(/^references Rudin 1976 \(Reference\) · accepted$/);
  });

  it("shows proof, verdict, and freshness in the Verification table as status text with exact labels", async () => {
    useGraphPage.setState({ section: "mathematics" });
    render(<GraphPage />);
    fireEvent.click(await screen.findByRole("radio", { name: "Verification" }));
    const grid = await screen.findByRole("grid", { name: "Verification" });
    const thmRow = await waitFor(() => grid.querySelector('[data-entity-id="thm"]') as HTMLElement);
    const lemRow = grid.querySelector('[data-entity-id="lem"]') as HTMLElement;
    const status = (scope: HTMLElement, label: string) => within(scope).getByText(label).closest(".ui-status-text")?.getAttribute("data-status");
    expect(status(thmRow, "No proof linked")).toBe("warning");
    expect(status(thmRow, "Not verified")).toBe("idle");
    expect(status(thmRow, "Unknown")).toBe("unavailable");
    expect(status(lemRow, "Proof linked")).toBe("idle");
    expect(status(lemRow, "Checked")).toBe("succeeded");
    expect(status(lemRow, "Current")).toBe("idle");
    // None / Not bound are muted text, not badges.
    expect(within(thmRow).getByText("None").closest(".ui-badge")).toBeNull();
    expect(within(thmRow).getByText("Not bound").classList.contains("ui-muted")).toBe(true);
    expect(grid.querySelectorAll(".ui-status-pill").length).toBe(0);
  });
});

describe("Mathematics node badges", () => {
  it("carry an icon with the exact verdict, freshness, and proof text", () => {
    const badges = mathBadges(row(thm, { informal: "noProof", verdict: "notVerified", freshness: { kind: "stale", reasons: ["paper.tex changed"] } }));
    expect(badges.map((b) => [b.label, b.tone, b.dashed ?? false])).toEqual([
      ["Not verified", "neutral", false],
      ["Stale evidence", "stale", true],
      ["No proof linked", "warning", false],
    ]);
    expect(badges.every((b) => b.icon)).toBe(true);
    const unknown = mathBadges(row(thm, { informal: "proofSuggested", verdict: "errors", freshness: { kind: "unknown", reason: "?" } }));
    expect(unknown.map((b) => [b.label, b.tone])).toEqual([
      ["Errors", "danger"],
      ["Freshness unknown", "neutral"],
      ["Proof suggested", "accent"],
    ]);
  });

  it("verdict tones: Checked succeeds, Errors fails, Not verified is neutral, qualified results warn", () => {
    expect(VERDICT_VIEW.checked.status).toBe("succeeded");
    expect(VERDICT_VIEW.errors.status).toBe("failed");
    expect(VERDICT_VIEW.notVerified.status).toBe("idle");
    for (const v of ["checkedWithAxioms", "incomplete", "reliesOnCompiledCode", "partiallyChecked"] as const) expect(VERDICT_VIEW[v].status).toBe("warning");
    expect(VERDICT_VIEW.incomplete.label).toBe("Incomplete (sorry)");
  });
});
