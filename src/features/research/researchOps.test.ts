// Research explorer operations: files and retained results are file targets,
// experiments are graph items, managed group folders and saved workflows are
// not deleted here (with the reason), and a group is a paste destination.
import { describe, expect, it, vi } from "vitest";

vi.mock("../operations/ipc", () => ({ opCommands: {}, opEvents: { projectOp: vi.fn(async () => () => {}) } }));
vi.mock("../workspace/fileOps", () => ({ copySelection: vi.fn(), createdPaths: () => [], pasteInto: vi.fn() }));
vi.mock("./space", () => ({
  parseItemKey: (key: string) => {
    const at = key.indexOf(":");
    const kind = key.slice(0, at);
    const rest = key.slice(at + 1);
    if (kind === "file") return { kind, path: rest };
    if (["experiment", "result", "reference", "workflow"].includes(kind)) return { kind, entity: rest };
    return null;
  },
}));

import type { ResearchListing } from "../../bindings/ResearchListing";
import { researchActions, resolveResearchKeys } from "./researchOps";

const listing = {
  space: { entity: "ent-space", spaceId: "s1", label: "Research", root: ".exposition/research/s1", createdAtMs: 0 },
  sources: [{ path: ".exposition/research/s1/sources/fit.py", name: "fit.py", group: "sources", sizeBytes: 10, entity: "ent-fit", roles: [], origin: null }],
  data: [],
  notes: [],
  references: [{ entity: "ent-ref", label: "paper.pdf" }],
  experiments: [{ entity: "ent-exp", label: "Exp A" }],
  results: [{ entity: "ent-res", label: "plot.png", path: ".exposition/research/s1/results/plot.png", lifecycle: "active", roles: [], runId: null, artifactId: null, kind: "figure", mediaType: null, contentHash: null, usedBy: [], experiments: [] }],
  workflows: [{ entity: "ent-wf", label: "Fit", lifecycle: "active" }],
  truncated: false,
  graphRevision: 1,
  graphUnavailable: null,
} as unknown as ResearchListing;

describe("research selection", () => {
  it("resolves files, results, experiments, groups; a deleted file no longer resolves", () => {
    const items = resolveResearchKeys(
      ["file:.exposition/research/s1/sources/fit.py", "result:ent-res", "experiment:ent-exp", "group:data", "file:.exposition/research/s1/sources/gone.py"],
      listing,
    );
    expect(items).toEqual([
      { kind: "file", path: ".exposition/research/s1/sources/fit.py", name: "fit.py" },
      { kind: "file", path: ".exposition/research/s1/results/plot.png", name: "plot.png" },
      { kind: "experiment", entity: "ent-exp", name: "Exp A" },
      { kind: "group", folder: ".exposition/research/s1/data", name: "data" },
    ]);
  });

  it("groups are paste destinations, not deletable; workflows are changed elsewhere", () => {
    const group = researchActions(resolveResearchKeys(["group:sources"], listing), { root: "C:/p" });
    expect(group.paste).toMatchObject({ enabled: true, label: "Paste into Sources" });
    expect(group.delete?.enabled).toBe(false);
    expect(group.delete?.reason).toMatch(/managed folders/);
    const wf = researchActions(resolveResearchKeys(["workflow:ent-wf"], listing), { root: "C:/p" });
    expect(wf.delete?.enabled).toBe(false);
    expect(wf.delete?.reason).toMatch(/Workflows tab/);
  });

  it("a file beside which to paste, and a mixed file + experiment deletion in one request", () => {
    const items = resolveResearchKeys(["file:.exposition/research/s1/sources/fit.py", "experiment:ent-exp"], listing);
    const actions = researchActions(items, { root: "C:/p" });
    expect(actions.delete?.enabled).toBe(true);
    expect(actions.copy?.enabled).toBe(false);
    expect(actions.paste?.enabled).toBe(false);
    const one = researchActions(resolveResearchKeys(["file:.exposition/research/s1/sources/fit.py"], listing), { root: "C:/p" });
    expect(one.paste?.label).toBe("Paste into Sources");
    expect(one.copy?.enabled).toBe(true);
  });
});
