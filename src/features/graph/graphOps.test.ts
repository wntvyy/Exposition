// Graph operation labels and availability (P1): specific labels, file-backed
// delete is the source file's delete, source-anchored items are neither
// deleted nor duplicated (Copy Reference / Copy Text instead), observed
// relationships follow their source, and the graph's read-only state blocks.
import { describe, expect, it, vi } from "vitest";

vi.mock("./reveal", () => ({ revealSource: vi.fn() }));
vi.mock("../operations/ipc", () => ({ opCommands: {}, opEvents: { projectOp: vi.fn(async () => () => {}) } }));

import { entity, fakeVocabulary, relation } from "./fakeApi";
import { classifyEntity, entityTarget, graphActions } from "./graphOps";
import { indexVocabulary } from "./model";

const vocabulary = indexVocabulary(fakeVocabulary());
const opts = { vocabulary, canWrite: true, onHide: () => {} };

const theorem = entity("thm", "theorem", "Theorem 2.1", { key: "thm:main", origin: { kind: "extracted", extractor: "latex" } });
const claim = entity("cl", "claim", "Conjecture A");
const file = entity("res", "resource", "plot.png", { key: "figures/plot.png", origin: { kind: "extracted", extractor: "files" } });
const folder = entity("dir", "folder", "chapters", { key: "chapters", origin: { kind: "extracted", extractor: "files" } });
const project = entity("prj", "project", "Paper");

describe("classification", () => {
  it("separates project, file-backed, source-anchored and manual items", () => {
    expect(classifyEntity(project, vocabulary)).toBe("project");
    expect(classifyEntity(file, vocabulary)).toBe("fileBacked");
    expect(classifyEntity(folder, vocabulary)).toBe("folder");
    expect(classifyEntity(theorem, vocabulary)).toBe("sourceAnchored");
    expect(classifyEntity(claim, vocabulary)).toBe("manual");
  });
});

describe("actions", () => {
  it("a file-backed resource: Delete Source File (to the trash) and Duplicate", () => {
    const { actions, extras } = graphActions({ entities: [file], relations: [] }, opts);
    expect(actions.delete).toMatchObject({ enabled: true, label: "Delete Source File" });
    expect(actions.delete?.description).toMatch(/moves to the Recycle Bin|moves to the Trash/);
    expect(actions.duplicate?.enabled).toBe(true);
    expect(extras.map((e) => e.id)).toEqual(["hide", "copyReference"]);
  });

  it("a source-anchored theorem: no delete or duplicate, with the reason; Copy Reference and Copy Text instead", () => {
    const { actions, extras } = graphActions({ entities: [theorem], relations: [] }, opts);
    expect(actions.delete?.enabled).toBe(false);
    expect(actions.delete?.reason).toMatch(/change the source/);
    expect(actions.duplicate?.enabled).toBe(false);
    expect(actions.duplicate?.reason).toMatch(/Copy Text or Copy Reference/);
    expect(extras.map((e) => e.label)).toEqual(["Hide from View", "Copy Reference", "Copy Text"]);
  });

  it("a manual item: Delete Entity; relationships: Delete Relationship; observed ones are blocked", () => {
    expect(graphActions({ entities: [claim], relations: [] }, opts).actions.delete).toMatchObject({ enabled: true, label: "Delete Entity" });
    const manual = relation("r1", "dependsOn", "thm", "cl");
    expect(graphActions({ entities: [], relations: [manual] }, opts).actions.delete).toMatchObject({ enabled: true, label: "Delete Relationship" });
    const observed = relation("r2", "references", "thm", "cl", "observed");
    const blocked = graphActions({ entities: [], relations: [observed] }, opts).actions.delete;
    expect(blocked?.enabled).toBe(false);
    expect(blocked?.reason).toMatch(/change the source/);
  });

  it("the project is never deleted; a read-only graph blocks changes", () => {
    expect(graphActions({ entities: [project], relations: [] }, opts).actions.delete?.enabled).toBe(false);
    const readOnly = graphActions({ entities: [claim], relations: [] }, { ...opts, canWrite: false }).actions;
    expect(readOnly.delete?.enabled).toBe(false);
    expect(readOnly.delete?.reason).toBe("The graph is read-only right now");
    expect(readOnly.duplicate?.enabled).toBe(false);
  });

  it("targets: a file-backed item IS its source file (a path target); a missing file is a record; workflows and references follow their own rules", () => {
    expect(entityTarget(file, classifyEntity(file, vocabulary))).toEqual({ kind: "path", path: "figures/plot.png" });
    expect(entityTarget(claim, classifyEntity(claim, vocabulary))).toEqual({ kind: "graphEntity", entity: "cl" });
    const gone = entity("gone", "resource", "old.png", { key: "old.png", lifecycle: "tombstoned", origin: { kind: "extracted", extractor: "files" } });
    expect(classifyEntity(gone, vocabulary)).toBe("missingFile");
    expect(entityTarget(gone, "missingFile")).toEqual({ kind: "graphEntity", entity: "gone" });
    const wf = graphActions({ entities: [entity("w", "workflow", "Fit")], relations: [] }, opts).actions;
    expect(wf.delete?.enabled).toBe(false);
    expect(wf.delete?.reason).toMatch(/Workflows list/);
    const ref = graphActions({ entities: [entity("r", "reference", "Rudin 1976", { key: "rudin" })], relations: [] }, opts);
    expect(ref.actions.delete?.enabled).toBe(true);
    expect(ref.actions.duplicate?.enabled).toBe(false);
    expect(ref.extras.map((e) => e.id)).toContain("copyReference");
  });

  it("a mixed selection uses one combined label", () => {
    expect(graphActions({ entities: [claim, file], relations: [] }, opts).actions.delete?.label).toBe("Delete Selected Items");
  });
});
