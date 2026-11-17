// The outline store drops answers that were overtaken: a newer request, a
// different document or path, another project, or an older revision.
import { describe, expect, it, vi } from "vitest";

vi.mock("../../lib/ipc", () => ({ commands: {}, errorMessage: (e: unknown) => String(e) }));

import type { DocumentOutline } from "../../bindings/DocumentOutline";
import { OutlineStore } from "./store";

function outline(path: string, revision: number): DocumentOutline {
  return { path, revision, unsaved: false, extractor: "typst/1", coverage: "complete", detail: null, items: [], itemCount: 0 };
}

/** A fetch whose answers the test releases in any order. */
function controlled() {
  const pending: Array<{ id: string; resolve: (o: DocumentOutline) => void; reject: (e: unknown) => void }> = [];
  const fetch = (id: string) =>
    new Promise<DocumentOutline>((resolve, reject) => {
      pending.push({ id, resolve, reject });
    });
  return { fetch, pending };
}

const a = { documentId: "doc-a", path: "a.typ", root: "C:/one" };
const b = { documentId: "doc-b", path: "b.typ", root: "C:/one" };

describe("OutlineStore", () => {
  it("keeps the newest answer when answers arrive out of order", async () => {
    const { fetch, pending } = controlled();
    const store = new OutlineStore(fetch);
    store.setTarget(a);
    const first = store.refresh();
    const second = store.refresh();
    pending[1]!.resolve(outline("a.typ", 3));
    await second;
    pending[0]!.resolve(outline("a.typ", 2));
    expect(await first).toBeNull();
    expect(store.getState().outline?.revision).toBe(3);
    expect(store.getState().loading).toBe(false);
  });

  it("ignores a late answer for the previous document", async () => {
    const { fetch, pending } = controlled();
    const store = new OutlineStore(fetch);
    store.setTarget(a);
    const late = store.refresh();
    store.setTarget(b);
    expect(store.getState().outline).toBeNull();
    pending[0]!.resolve(outline("a.typ", 9));
    expect(await late).toBeNull();
    expect(store.getState()).toMatchObject({ target: b, outline: null });
  });

  it("ignores a late answer after a project switch with the same file name", async () => {
    const { fetch, pending } = controlled();
    const store = new OutlineStore(fetch);
    store.setTarget(a);
    const late = store.refresh();
    store.setTarget({ ...a, root: "C:/two" });
    pending[0]!.resolve(outline("a.typ", 1));
    expect(await late).toBeNull();
    expect(store.getState().outline).toBeNull();
  });

  it("never replaces a newer outline with an older revision", async () => {
    const { fetch, pending } = controlled();
    const store = new OutlineStore(fetch);
    store.setTarget(a);
    const one = store.refresh();
    pending[0]!.resolve(outline("a.typ", 5));
    await one;
    const two = store.refresh();
    pending[1]!.resolve(outline("a.typ", 4));
    expect(await two).toBeNull();
    expect(store.getState().outline?.revision).toBe(5);
  });

  it("reports failures for the current request only", async () => {
    const { fetch, pending } = controlled();
    const store = new OutlineStore(fetch);
    store.setTarget(a);
    const old = store.refresh();
    store.setTarget(b);
    pending[0]!.reject(new Error("gone"));
    await old;
    expect(store.getState().error).toBeNull();
    const current = store.refresh();
    pending[1]!.reject("document.unknown");
    await current;
    expect(store.getState().error).toBe("document.unknown");
  });

  it("does nothing without a target and keeps the target when set again", async () => {
    const { fetch, pending } = controlled();
    const store = new OutlineStore(fetch);
    expect(await store.refresh()).toBeNull();
    expect(pending).toHaveLength(0);
    store.setTarget(a);
    const one = store.refresh();
    pending[0]!.resolve(outline("a.typ", 1));
    await one;
    store.setTarget({ ...a });
    expect(store.getState().outline?.revision).toBe(1);
  });
});
