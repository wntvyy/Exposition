import { describe, expect, it, vi } from "vitest";

import { listNames, notSavedMessage, switchProject, type DirtyItem, type SwitchDeps } from "./switch";

const doc = (name: string, id = `doc-${name}`): DirtyItem => ({ key: `file:${name}`, name, document: id });
const draft = (name: string): DirtyItem => ({ key: `workflow:${name}`, name, document: null });

function deps(overrides: Partial<SwitchDeps> = {}, current: string | null = "alpha") {
  const log: string[] = [];
  const d: SwitchDeps = {
    currentName: () => current,
    dirtyWork: vi.fn(async () => []),
    activeWork: vi.fn(async () => []),
    decide: vi.fn(async () => "cancel" as const),
    saveDocuments: vi.fn(async () => {
      log.push("saveDocuments");
      return true;
    }),
    saveEditor: vi.fn(async (key: string) => {
      log.push(`saveEditor:${key}`);
      return true;
    }),
    discardEditors: vi.fn(async (keys: string[]) => {
      log.push(`discardEditors:${keys.join(",")}`);
    }),
    ...overrides,
  };
  return { d, log };
}

function target(commit: (discard: string[]) => Promise<string> = async () => "beta-info", log?: string[]) {
  return {
    action: "open" as const,
    name: "beta",
    commit: vi.fn(async (discard: string[]) => {
      log?.push(`commit:${discard.join(",")}`);
      return commit(discard);
    }),
  };
}

describe("switchProject", () => {
  it("opens directly when no project is open (nothing to guard)", async () => {
    const { d } = deps({}, null);
    const t = target();
    expect(await switchProject(t, d)).toEqual({ status: "switched", value: "beta-info", discarded: [] });
    expect(d.dirtyWork).not.toHaveBeenCalled();
    expect(d.decide).not.toHaveBeenCalled();
  });

  it("switches without asking when nothing is unsaved or running", async () => {
    const { d } = deps();
    const t = target();
    expect((await switchProject(t, d)).status).toBe("switched");
    expect(d.decide).not.toHaveBeenCalled();
    expect(t.commit).toHaveBeenCalledWith([]);
  });

  it("Cancel changes nothing: no save, no commit, no discard", async () => {
    const { d } = deps({ dirtyWork: async () => [doc("main.typ")], decide: vi.fn(async () => "cancel" as const) });
    const t = target();
    expect(await switchProject(t, d)).toEqual({ status: "canceled" });
    expect(t.commit).not.toHaveBeenCalled();
    expect(d.saveDocuments).not.toHaveBeenCalled();
    expect(d.discardEditors).not.toHaveBeenCalled();
  });

  it("asks once with the unsaved and running work of the open project", async () => {
    const decide = vi.fn(async () => "cancel" as const);
    const { d } = deps({
      dirtyWork: async () => [doc("main.typ"), draft("Simulate")],
      activeWork: async () => [{ kind: "workflowRun", name: "Nightly" }],
      decide,
    });
    await switchProject(target(), d);
    expect(decide).toHaveBeenCalledTimes(1);
    expect(decide).toHaveBeenCalledWith({
      action: "open",
      from: "alpha",
      to: "beta",
      dirty: [doc("main.typ"), draft("Simulate")],
      active: [{ kind: "workflowRun", name: "Nightly" }],
    });
  });

  it("Save All saves documents and drafts, then commits discarding nothing", async () => {
    const dirty = vi.fn<() => Promise<DirtyItem[]>>().mockResolvedValueOnce([doc("main.typ"), draft("Simulate")]).mockResolvedValue([]);
    const { d, log } = deps({ dirtyWork: dirty, decide: async () => "save" });
    const t = target(undefined, log);
    expect((await switchProject(t, d)).status).toBe("switched");
    expect(log).toEqual(["saveDocuments", "saveEditor:workflow:Simulate", "commit:"]);
    expect(d.discardEditors).not.toHaveBeenCalled();
  });

  it("a failed save keeps the project open and reports the actual partial state", async () => {
    const dirty = vi
      .fn<() => Promise<DirtyItem[]>>()
      .mockResolvedValueOnce([doc("a.tex"), doc("b.tex"), draft("Simulate")])
      .mockResolvedValue([doc("b.tex")]);
    const { d } = deps({ dirtyWork: dirty, decide: async () => "save", saveDocuments: async () => false });
    const t = target();
    const result = await switchProject(t, d);
    expect(result).toEqual({ status: "notSaved", saved: [doc("a.tex"), draft("Simulate")], unsaved: [doc("b.tex")] });
    expect(t.commit).not.toHaveBeenCalled();
    if (result.status === "notSaved") {
      expect(notSavedMessage(result, "alpha")).toBe("Saved: a.tex and Simulate. Not saved: b.tex. alpha is still open.");
    }
  });

  it("a draft that could not be saved stops the switch too", async () => {
    const dirty = vi.fn<() => Promise<DirtyItem[]>>().mockResolvedValueOnce([draft("Simulate")]).mockResolvedValue([draft("Simulate")]);
    const { d } = deps({ dirtyWork: dirty, decide: async () => "save", saveEditor: async () => false });
    const t = target();
    expect((await switchProject(t, d)).status).toBe("notSaved");
    expect(t.commit).not.toHaveBeenCalled();
    expect(d.saveDocuments).not.toHaveBeenCalled();
  });

  it("Save All whose save reloaded from disk (nothing left unsaved) proceeds", async () => {
    const dirty = vi.fn<() => Promise<DirtyItem[]>>().mockResolvedValueOnce([doc("a.tex")]).mockResolvedValue([]);
    const { d } = deps({ dirtyWork: dirty, decide: async () => "save", saveDocuments: async () => false });
    expect((await switchProject(target(), d)).status).toBe("switched");
  });

  it("Don't Save passes the documents to the commit and closes discarded drafts only after it succeeded", async () => {
    const { d, log } = deps({ dirtyWork: async () => [doc("main.typ", "d1"), draft("Simulate")], decide: async () => "discard" });
    const t = target(undefined, log);
    const result = await switchProject(t, d);
    expect(result).toMatchObject({ status: "switched", discarded: [doc("main.typ", "d1"), draft("Simulate")] });
    expect(log).toEqual(["commit:d1", "discardEditors:workflow:Simulate"]);
    expect(d.saveDocuments).not.toHaveBeenCalled();
  });

  it("a destination that fails to open after Don't Save discards nothing", async () => {
    const { d, log } = deps({ dirtyWork: async () => [doc("main.typ", "d1"), draft("Simulate")], decide: async () => "discard" });
    const error = { code: "workspace.notFound", message: "C:\\gone does not exist." };
    const t = target(async () => Promise.reject(error), log);
    expect(await switchProject(t, d)).toEqual({ status: "failed", error });
    // The backend refuses before closing anything; the drafts stay open and unsaved.
    expect(log).toEqual(["commit:d1"]);
    expect(d.discardEditors).not.toHaveBeenCalled();
  });

  it("a destination that fails after Save All reports failure (the saves stand)", async () => {
    const dirty = vi.fn<() => Promise<DirtyItem[]>>().mockResolvedValueOnce([doc("a.tex")]).mockResolvedValue([]);
    const { d } = deps({ dirtyWork: dirty, decide: async () => "save" });
    const t = target(async () => Promise.reject(new Error("refused")));
    expect((await switchProject(t, d)).status).toBe("failed");
    expect(d.saveDocuments).toHaveBeenCalledTimes(1);
  });

  it("running work alone asks; Stop and Open commits, Cancel does not", async () => {
    const active = async () => [{ kind: "research" as const, name: "Python 3.13" }];
    const stop = deps({ activeWork: active, decide: async () => "proceed" });
    const t = target();
    expect((await switchProject(t, stop.d)).status).toBe("switched");
    expect(t.commit).toHaveBeenCalledWith([]);
    const keep = deps({ activeWork: active, decide: async () => "cancel" });
    const t2 = target();
    expect((await switchProject(t2, keep.d)).status).toBe("canceled");
    expect(t2.commit).not.toHaveBeenCalled();
  });
});

describe("messages", () => {
  it("lists names", () => {
    expect(listNames([])).toBe("");
    expect(listNames(["a"])).toBe("a");
    expect(listNames(["a", "b", "c"])).toBe("a, b and c");
  });

  it("omits the saved part when nothing was saved", () => {
    expect(notSavedMessage({ saved: [], unsaved: [doc("a.tex")] }, "alpha")).toBe("Not saved: a.tex. alpha is still open.");
  });
});
