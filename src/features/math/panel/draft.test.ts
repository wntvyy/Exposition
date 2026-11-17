// The equation draft against a document wired like DocumentHandle (real
// UndoGrouper + DocumentSync): what is written, as how many transactions,
// and what is never written (cancel, unchanged, conflicts, refusals).
// The math core is the test fake (lane EQ-A's core replaces it at merge).
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../core", async () => await import("./testing/fakeCore"));

import { insertSymbol, move, typeText } from "../core";
import { resolvePageCaret, writePatches, type CaretTarget, type Resolved } from "./document";
import { EquationPanelStore, type DraftEnvironment } from "./draft";
import { fakeDocument, type FakeDocument } from "./testing/fakeDocument";

const flush = () => new Promise((r) => setTimeout(r, 0));

const LATEX = [
  "\\documentclass{article}",
  "\\begin{document}",
  "Let $x+1$ be small.",
  "\\begin{equation}\\label{eq:one}a=b\\end{equation}",
  "See \\ref{eq:one}.",
  "% a comment",
  "\\end{document}",
].join("\n");

function setup(text = LATEX, path = "main.tex") {
  const doc = fakeDocument(text, { path });
  const docs: FakeDocument[] = [doc];
  let next: Resolved = { error: "no caret" };
  const env: DraftEnvironment = {
    resolve: vi.fn(async () => next),
    document: (id) => docs.find((d) => d.id === id && !d.closed),
    showPanel: vi.fn(),
    copy: vi.fn(async () => true),
  };
  const store = new EquationPanelStore(env);
  const at = (start: number, end = start, target: FakeDocument = doc, surface: "source" | "page" = "source"): CaretTarget => {
    const caret: CaretTarget = { surface, document: target, path: target.path!, start, end, focus: vi.fn() };
    next = caret;
    return caret;
  };
  return { doc, docs, env, store, at, text: () => doc.model.getValue() };
}

describe("equation draft: writing", () => {
  let s: ReturnType<typeof setup>;
  beforeEach(() => {
    s = setup();
  });

  it("edits an existing equation as one transaction, keeping delimiters, labels, and references", async () => {
    s.at(LATEX.indexOf("a=b") + 1);
    expect(await s.store.open("auto")).toBe(true);
    const draft = s.store.getState().draft!;
    expect(draft.kind).toBe("edit");
    expect(draft.complete).toBe(false); // \label is kept as an opaque segment
    s.store.edit((st) => typeText(st, "+c"));
    expect(await s.store.apply()).toBe("written");
    expect(s.text()).toContain("\\begin{equation}\\label{eq:one}a=b+c\\end{equation}");
    expect(s.text()).toContain("See \\ref{eq:one}.");
    expect(s.doc.requests).toHaveLength(1);
    const request = s.doc.requests[0]!;
    expect(request.edits[0]!.patches).toHaveLength(1);
    expect(request.undo).toEqual({ kind: "newGroup" });
    expect(request.origin).toEqual({ kind: "source" });
    expect(s.store.getState().draft).toBeNull();
  });

  it("creates an inline equation in text with the language's delimiters", async () => {
    const at = LATEX.indexOf("be small") + 3;
    s.at(at);
    await s.store.open("inline");
    expect(s.store.getState().draft).toMatchObject({ kind: "insert", context: "text", display: false });
    s.store.edit((st) => typeText(st, "y"));
    await s.store.apply();
    expect(s.text().slice(at, at + 3)).toBe("$y$");
    expect(s.doc.requests).toHaveLength(1);
  });

  it("creates a display equation when asked", async () => {
    const at = LATEX.indexOf("See");
    s.at(at);
    await s.store.open("display");
    s.store.edit((st) => typeText(st, "z"));
    await s.store.apply();
    expect(s.text().slice(at, at + 7)).toBe("\\[\nz\n\\]");
  });

  it("inserts inside math without new delimiters", async () => {
    const at = LATEX.indexOf("x+1") + 1;
    s.at(at);
    await s.store.open("inline");
    expect(s.store.getState().draft).toMatchObject({ kind: "insert", context: "math" });
    s.store.edit((st) => insertSymbol(st, "greek.alpha"));
    await s.store.apply();
    expect(s.text()).toContain("Let $x\\alpha+1$ be small.");
  });

  it("makes a selection in text the new equation's content", async () => {
    const at = LATEX.indexOf("be small");
    s.at(at, at + 2);
    await s.store.open("inline");
    expect(s.store.getState().draft!.initial.children).toHaveLength(2);
    await s.store.apply();
    expect(s.text()).toContain("Let $x+1$ $be$ small.");
  });

  it("writes nothing for an unchanged equation, a cancel, or an empty new equation", async () => {
    s.at(LATEX.indexOf("a=b"));
    await s.store.open("edit");
    expect(await s.store.apply()).toBe("unchanged");
    s.at(LATEX.indexOf("x+1"));
    await s.store.open("edit");
    s.store.edit((st) => typeText(st, "q"));
    s.store.cancel();
    s.at(LATEX.indexOf("See"));
    await s.store.open("inline");
    expect(await s.store.apply()).toBe("unchanged");
    await flush();
    expect(s.doc.requests).toHaveLength(0);
    expect(s.text()).toBe(LATEX);
  });

  it("adds a chosen requirement in the same transaction", async () => {
    s.at(LATEX.indexOf("a=b") + 3);
    await s.store.open("edit");
    s.store.edit((st) => insertSymbol(move(st, "end"), "rel.lesssim"));
    const analysis = s.store.analyze()!;
    expect(analysis.missing.map((m) => m.requirement.name)).toEqual(["amssymb"]);
    expect(s.store.getState().draft!.addRequirements).toBe(true);
    await s.store.apply();
    expect(s.doc.requests).toHaveLength(1);
    expect(s.doc.requests[0]!.edits[0]!.patches).toHaveLength(2);
    expect(s.text()).toContain("\\usepackage{amssymb}\n\\begin{document}");
    expect(s.text()).toContain("a=b\\lesssim\\end{equation}");
  });

  it("leaves the preamble alone when the requirement is declined", async () => {
    s.at(LATEX.indexOf("a=b") + 3);
    await s.store.open("edit");
    s.store.edit((st) => insertSymbol(st, "rel.lesssim"));
    s.store.setAddRequirements(false);
    await s.store.apply();
    expect(s.doc.requests[0]!.edits[0]!.patches).toHaveLength(1);
    expect(s.text()).not.toContain("amssymb");
  });

  it("refuses to write parts the language cannot express", async () => {
    s.at(LATEX.indexOf("a=b"));
    await s.store.open("edit");
    s.store.edit((st) => insertSymbol(st, "misc.typstonly"));
    expect(s.store.analyze()!.serialized!.unsupported).toHaveLength(1);
    expect(await s.store.apply()).toBe("blocked");
    expect(s.doc.requests).toHaveLength(0);
    expect(s.store.getState().draft).not.toBeNull();
  });

  it("never merges the equation write into typing before or after it", async () => {
    const at = LATEX.indexOf("x+1") + 3;
    s.at(at);
    await s.store.open("inline");
    s.store.edit((st) => typeText(st, "2"));
    await s.store.apply();
    await flush();
    // The user types right after the inserted character within the typing pause.
    s.doc.model.edit([{ rangeOffset: at + 1, rangeLength: 0, text: "3" }]);
    await flush();
    const [equation, typed] = s.doc.requests;
    expect(equation!.undo.kind).toBe("merge");
    expect(typed!.undo.kind).toBe("merge");
    expect((typed!.undo as { key: string }).key).not.toBe((equation!.undo as { key: string }).key);
  });

  it("records the edited content range as the undo selection", async () => {
    s.at(LATEX.indexOf("a=b"));
    await s.store.open("edit");
    s.store.edit((st) => typeText(st, "1"));
    await s.store.apply();
    const start = LATEX.indexOf("\\label{eq:one}a=b");
    expect(s.doc.requests[0]!.selection!.before[0]).toEqual({ start, end: start + "\\label{eq:one}a=b".length });
  });
});

describe("equation draft: target safety", () => {
  let s: ReturnType<typeof setup>;
  beforeEach(() => {
    s = setup();
  });

  it("follows edits outside the target and writes at the shifted position", async () => {
    s.at(LATEX.indexOf("a=b"));
    await s.store.open("edit");
    s.store.edit((st) => typeText(st, "+d"));
    s.doc.model.edit([{ rangeOffset: LATEX.indexOf("Let"), rangeLength: 0, text: "Now " }]);
    expect(await s.store.apply()).toBe("written");
    expect(s.text()).toContain("Now Let $x+1$");
    expect(s.text()).toContain("\\label{eq:one}a=b+d\\end{equation}");
  });

  it("shows a conflict and writes nothing when the equation changed during the draft", async () => {
    s.at(LATEX.indexOf("a=b"));
    await s.store.open("edit");
    s.store.edit((st) => typeText(st, "+d"));
    // An agent (or the user in the source) edits the same equation.
    s.doc.model.edit([{ rangeOffset: LATEX.indexOf("a=b"), rangeLength: 1, text: "A" }]);
    expect(s.store.getState().draft!.conflict).toEqual({ kind: "changed" });
    await flush();
    const before = s.text();
    const requests = s.doc.requests.length;
    expect(await s.store.apply()).toBe("conflict");
    await flush();
    expect(s.text()).toBe(before);
    expect(s.doc.requests).toHaveLength(requests);
    // The draft is kept, and can be re-targeted to the equation now there.
    expect(s.store.currentEquation()).not.toBeNull();
    expect(s.store.retargetToCurrentEquation()).toBe(true);
    expect(s.store.getState().draft!.conflict).toBeNull();
    expect(await s.store.apply()).toBe("written");
    expect(s.text()).toContain("\\begin{equation}\\label{eq:one}a=b+d\\end{equation}");
  });

  it("treats a closed document as a conflict and keeps the draft for copying", async () => {
    s.at(LATEX.indexOf("a=b"));
    await s.store.open("edit");
    s.store.edit((st) => typeText(st, "+d"));
    s.doc.closed = true;
    s.doc.model.dispose();
    expect(await s.store.apply()).toBe("conflict");
    expect(s.store.getState().draft!.conflict?.kind).toBe("closed");
    expect(await s.store.copyDraft()).toBe(true);
    expect(s.env.copy).toHaveBeenCalledWith("\\label{eq:one}a=b+d");
  });

  it("keeps a changed draft when another open is requested, and moves it only on retarget", async () => {
    s.at(LATEX.indexOf("a=b"));
    await s.store.open("edit");
    s.store.edit((st) => typeText(st, "+d"));
    const other = fakeDocument("Intro $p$ end.", { id: "doc-2", path: "notes.md" });
    s.docs.push(other);
    s.at(0, 0, other);
    expect(await s.store.open("inline")).toBe(false);
    expect(s.store.getState().draft!.path).toBe("main.tex");
    expect(s.store.getState().notice?.text).toMatch(/Apply or discard/);
    // Different language: the draft is not moved there.
    expect(await s.store.retargetToCaret()).toBe(false);
    const tex2 = fakeDocument("\\begin{document}\nB $q$\n\\end{document}", { id: "doc-3", path: "other.tex" });
    s.docs.push(tex2);
    s.at(tex2.model.getValue().indexOf("q"), undefined, tex2);
    expect(await s.store.retargetToCaret()).toBe(true);
    expect(s.store.getState().draft).toMatchObject({ path: "other.tex", kind: "edit" });
    await s.store.apply();
    expect(tex2.model.getValue()).toContain("$\\label{eq:one}a=b+d$");
    expect(s.text()).toBe(LATEX);
  });

  it("refuses targets it cannot write: comments, crossing selections, read-only documents", async () => {
    s.at(LATEX.indexOf("a comment"));
    expect(await s.store.open("inline")).toBe(false);
    expect(s.store.getState().notice?.text).toMatch(/comment/);
    s.at(LATEX.indexOf("x+1"), LATEX.indexOf("be small"));
    expect(await s.store.open("inline")).toBe(false);
    expect(s.store.getState().notice?.text).toMatch(/crosses an equation boundary/);
    s.doc.editReason = "This file mixes line endings. Normalize them to edit it.";
    s.at(LATEX.indexOf("See"));
    expect(await s.store.open("inline")).toBe(false);
    expect(s.store.getState().notice?.text).toMatch(/mixes line endings/);
    expect(s.store.getState().draft).toBeNull();
  });

  it("refuses Edit Equation outside an equation", async () => {
    s.at(LATEX.indexOf("See"));
    expect(await s.store.open("edit")).toBe(false);
    expect(s.store.getState().notice?.text).toBe("The caret is not inside an equation.");
  });
});

describe("equation draft: symbols and history", () => {
  it("inserts a single symbol at the caret directly, as one transaction", async () => {
    const s = setup("Intro text $p$ end.", "notes.md");
    s.at(6);
    expect(await s.store.insertSymbolAtCaret("greek.alpha", "page")).toBe("written");
    expect(s.text()).toBe("Intro $\\alpha$text $p$ end.");
    expect(s.doc.requests).toHaveLength(1);
    expect(s.store.getState().draft).toBeNull();
  });

  it("opens a draft instead when the symbol needs document configuration", async () => {
    const s = setup();
    s.at(LATEX.indexOf("x+1"));
    expect(await s.store.insertSymbolAtCaret("rel.lesssim")).toBe("draft");
    expect(s.store.analyze()!.missing).toHaveLength(1);
    expect(s.doc.requests).toHaveLength(0);
  });

  it("inserts into the open draft at its caret", async () => {
    const s = setup();
    s.at(LATEX.indexOf("x+1"));
    await s.store.open("edit");
    await s.store.insertSymbolAtCaret("greek.beta");
    const root = s.store.getState().draft!.editor.root;
    expect(root.children.at(-1)).toEqual({ kind: "symbol", id: "greek.beta" });
    expect(s.doc.requests).toHaveLength(0);
  });

  it("undoes and redoes inside the draft without touching the document", async () => {
    const s = setup();
    s.at(LATEX.indexOf("x+1"));
    await s.store.open("edit");
    s.store.edit((st) => typeText(st, "a"));
    s.store.edit((st) => typeText(st, "b"));
    s.store.undo();
    expect(s.store.getState().draft!.editor.root.children).toHaveLength(4);
    s.store.redo();
    expect(s.store.getState().draft!.editor.root.children).toHaveLength(5);
    expect(s.doc.requests).toHaveLength(0);
  });
});

describe("page caret resolution", () => {
  it("uses the page caret only when it is in the model's current revision", async () => {
    const doc = fakeDocument("Hello $x$");
    const surface = { path: "main.tex", focus: vi.fn(), currentCaret: vi.fn(() => ({ offset: 3, anchor: null, revision: 1, mode: null })) };
    expect(await resolvePageCaret(surface, doc)).toEqual({ caret: { offset: 3, anchor: null, revision: 1, mode: null } });
    // The page is a revision ahead of the model (an event not applied yet).
    surface.currentCaret.mockReturnValue({ offset: 3, anchor: null, revision: 2, mode: null });
    expect(await resolvePageCaret(surface, doc)).toEqual({
      error: "The page has not caught up with the latest change to the source yet. Try again in a moment.",
    });
    surface.currentCaret.mockReturnValue(null as never);
    expect("error" in (await resolvePageCaret(surface, doc))).toBe(true);
  });

  it("waits for unacknowledged source edits before agreeing", async () => {
    const doc = fakeDocument("Hello $x$");
    doc.model.edit([{ rangeOffset: 0, rangeLength: 0, text: "A" }]);
    expect(doc.sync.pendingCount).toBe(1);
    const surface = { path: "main.tex", focus: vi.fn(), currentCaret: () => ({ offset: 4, anchor: null, revision: doc.sync.revision, mode: null }) };
    const resolved = await resolvePageCaret(surface, doc);
    expect(resolved).toEqual({ caret: { offset: 4, anchor: null, revision: 2, mode: null } });
  });

  it("writePatches refuses overlapping patches and blocked documents without writing", () => {
    const doc = fakeDocument("abcdef");
    expect(() => writePatches(doc, [{ start: 1, end: 3, text: "x" }, { start: 2, end: 4, text: "y" }], { start: 1, end: 3 })).toThrow();
    doc.editReason = "Read-only";
    expect(() => writePatches(doc, [{ start: 0, end: 0, text: "x" }], { start: 0, end: 0 })).toThrow("Read-only");
    expect(doc.model.getValue()).toBe("abcdef");
    expect(doc.model.applyEditsCalls).toBe(0);
  });
});
