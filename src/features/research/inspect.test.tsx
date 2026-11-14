// Research run history and graph inspection: runs are rows that select by
// click and by Enter/Space; relations show settled statuses as muted text and
// suggestions as provisional (dashed) badges; evidence freshness "Current" is
// neutral, never success.
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { EntityDetail } from "../../bindings/EntityDetail";
import type { RunSummary } from "../../bindings/RunSummary";

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

const graph = vi.hoisted(() => ({ entity: vi.fn(), impact: vi.fn() }));
const store = vi.hoisted(() => ({ selectRun: vi.fn() }));
vi.mock("../../lib/graphIpc", () => ({ graphCommands: graph, graphEvents: { graph: vi.fn(), graphResync: vi.fn() } }));
vi.mock("./store", async (importOriginal) => ({ ...(await importOriginal<typeof import("./store")>()), ...store }));

const { useResearch } = await import("./store");
const { RunHistory, RunRow } = await import("./ResearchPanel");
const { GraphDependencies, freshnessStatus } = await import("./GraphInspect");

function run(id: string, code: string): RunSummary {
  return {
    id,
    codePreview: code,
    status: { kind: "succeeded" },
    queued: false,
    startedAtMs: 1_000,
    finishedAtMs: 1_700,
    owner: { kind: "user" },
    outputs: [{ kind: "figure" }],
  } as unknown as RunSummary;
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("RunHistory", () => {
  it("lists runs as rows that select on click and from the keyboard", () => {
    useResearch.setState({ runs: [run("run-1", "fit(x)"), run("run-2", "plot(y)")], selectedRun: "run-1" });
    render(<RunHistory />);
    const history = screen.getByTestId("research-history");
    const first = history.querySelector<HTMLElement>('[data-run-id="run-1"]')!;
    const second = history.querySelector<HTMLElement>('[data-run-id="run-2"]')!;
    expect(first.getAttribute("role")).toBe("button");
    expect(first.getAttribute("aria-current")).toBe("true");
    expect(second.getAttribute("aria-current")).toBeNull();
    expect(within(second).getByText("Executed").closest(".ui-status-text")).not.toBeNull();
    expect(second.textContent).toContain("figure");

    fireEvent.click(second);
    expect(store.selectRun).toHaveBeenLastCalledWith("run-2");
    fireEvent.keyDown(first, { key: "Enter" });
    expect(store.selectRun).toHaveBeenLastCalledWith("run-1");
    fireEvent.keyDown(second, { key: " " });
    expect(store.selectRun).toHaveBeenLastCalledWith("run-2");
    expect(store.selectRun).toHaveBeenCalledTimes(3);
  });
});

describe("RunRow", () => {
  it("joins its facts without a dangling separator when the title replaces the time", () => {
    const attempt = { ...run("run-3", "fit(x)"), owner: { kind: "agent" } } as unknown as RunSummary;
    const { container } = render(
      <ul>
        <RunRow run={attempt} now={2_000} title="10:00:00" onSelect={() => {}} />
      </ul>,
    );
    const facts = container.querySelector(".ui-list-row__meta > span:last-child")!;
    expect(facts.textContent).toBe("agent · figure");
  });
});

describe("GraphDependencies", () => {
  it("quiets settled relation statuses, marks suggestions provisional, and keeps Current neutral", async () => {
    const detail = {
      entity: { id: "e-fig", label: "fig.png" },
      outgoing: [
        { id: "r1", kind: "derivedFrom", from: "e-fig", to: "e-data", status: "accepted", note: null },
        { id: "r2", kind: "evidenceFor", from: "e-fig", to: "e-claim", status: "suggested", note: null },
      ],
      incoming: [],
      neighbors: [
        { id: "e-data", label: "obs.csv" },
        { id: "e-claim", label: "Slope is positive" },
      ],
      evidence: [
        {
          id: "ev-1",
          kind: "computational",
          operation: { kind: "run", id: "run-1" },
          outcome: { kind: "succeeded" },
          inputs: [],
          outputs: [],
          environment: [],
        },
      ],
      evidenceFreshness: [["ev-1", { kind: "current" }]],
      graphRevision: 7,
    } as unknown as EntityDetail;
    graph.entity.mockResolvedValue(detail);
    graph.impact.mockResolvedValue({ items: [], truncated: false, workLimitReached: false, unknownCoverage: [] });
    render(<GraphDependencies entity="e-fig" />);
    await waitFor(() => expect(screen.getByTestId("graph-dependencies")).toBeTruthy());

    expect(screen.getByRole("heading", { name: "Relations 2" })).toBeTruthy();
    const [accepted, suggested] = screen.getAllByTestId("graph-relation");
    expect(accepted!.textContent).toBe("fig.png is derived from obs.csv · accepted");
    expect(accepted!.querySelector(".ui-badge")).toBeNull();
    const badge = suggested!.querySelector(".ui-badge")!;
    expect(badge.textContent).toBe("Suggested");
    expect(badge.hasAttribute("data-dashed")).toBe(true);

    const evidence = screen.getByTestId("graph-evidence");
    expect(evidence.getAttribute("data-freshness")).toBe("current");
    const fresh = within(evidence).getByText("Current").closest(".ui-status-text")!;
    expect(fresh.getAttribute("data-status")).toBe("idle");
    expect(freshnessStatus({ kind: "stale", reasons: ["obs.csv changed"] })).toMatchObject({ status: "stale", label: "Stale (history)" });
  });
});
