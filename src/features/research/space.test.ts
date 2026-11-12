// Research space store: explorer keys round-trip, results are found by path,
// enabling refreshes the listing, and an experiment's procedure runs with its
// declared inputs pinned.
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ExperimentDetail } from "../../bindings/ExperimentDetail";
import type { ResearchListing } from "../../bindings/ResearchListing";

const research = vi.hoisted(() => ({ enable: vi.fn(), listing: vi.fn(), promotions: vi.fn() }));
const store = vi.hoisted(() => ({ execute: vi.fn() }));
vi.mock("./ipc", () => ({ researchCommands: research }));
vi.mock("./store", () => store);
vi.mock("../../lib/ipc", () => ({
  commands: {},
  events: {},
  errorMessage: (e: unknown) => String(e),
  isCommandError: () => false,
}));
vi.mock("../../lib/graphIpc", () => ({ graphCommands: {}, graphEvents: {} }));
vi.mock("../editor/documentClient", () => ({ fileKey: (p: string) => `file:${p}`, handleForPath: vi.fn(), saveDocument: vi.fn() }));

const { itemKey, parseItemKey, resultForPath, enableResearch, runExperiment, useSpace } = await import("./space");

const listing = {
  space: { entity: "ent-s", spaceId: "rs-1", label: "Research", root: ".exposition/research/rs-1", createdAtMs: 0 },
  sources: [],
  data: [],
  notes: [],
  references: [],
  experiments: [],
  results: [{ entity: "ent-r", label: "output-1.png", path: ".exposition/artifacts/run-1/output-1.png", lifecycle: "active" }],
  workflows: [],
  truncated: false,
  graphRevision: 3,
  graphUnavailable: null,
} as unknown as ResearchListing;

afterEach(() => vi.clearAllMocks());

describe("research space store", () => {
  it("round-trips explorer keys", () => {
    for (const item of [
      { kind: "file", path: ".exposition/research/rs-1/sources/a b.py" },
      { kind: "result", entity: "ent-1" },
      { kind: "experiment", entity: null },
      { kind: "experiment", entity: "ent-2" },
      { kind: "workflow", entity: "ent-3" },
      { kind: "reference", entity: "ent-4" },
      { kind: "run", runId: "run-abc" },
    ] as const) {
      expect(parseItemKey(itemKey(item))).toEqual(item);
    }
    expect(parseItemKey("group:sources")).toBeNull();
  });

  it("finds a retained result by its artifact path", () => {
    expect(resultForPath(listing, ".exposition/artifacts/run-1/output-1.png")?.entity).toBe("ent-r");
    expect(resultForPath(listing, ".exposition/artifacts/run-1/output-2.csv")).toBeUndefined();
    expect(resultForPath(null, "x")).toBeUndefined();
  });

  it("enabling Research refreshes the listing", async () => {
    research.enable.mockResolvedValue(listing.space);
    research.listing.mockResolvedValue(listing);
    expect(await enableResearch()).toBe(true);
    expect(research.enable).toHaveBeenCalledTimes(1);
    expect(useSpace.getState().listing?.space?.spaceId).toBe("rs-1");
    expect(useSpace.getState().enabling).toBe(false);
  });

  it("runs an experiment's procedure with its declared inputs", async () => {
    const detail = {
      procedure: { path: ".exposition/research/rs-1/sources/fit.py", entity: "ent-p" },
      inputs: [{ path: ".exposition/research/rs-1/data/obs.csv", entity: "ent-d" }],
    } as unknown as ExperimentDetail;
    await runExperiment(detail);
    expect(store.execute).toHaveBeenCalledWith(
      { kind: "file", path: ".exposition/research/rs-1/sources/fit.py" },
      [".exposition/research/rs-1/data/obs.csv"],
    );
  });
});
