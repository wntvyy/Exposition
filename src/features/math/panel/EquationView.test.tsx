// The Equation panel: keyboard editing in the structural editor, slots,
// templates, Enter/Escape rules, symbol search (names, synonyms, glyphs,
// commands) with keyboard navigation and empty states, focus retention when
// inserting with the mouse, and the notices (requirements, conflicts, kept
// source, another document). The core is the test fake (EQ-A's core at merge).
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("../core", async () => await import("./testing/fakeCore"));

import { insertSymbol, insertTemplate } from "../core";
import type { CaretTarget, Resolved } from "./document";
import { EquationPanelStore } from "./draft";
import { EquationView } from "./EquationView";
import { fakeDocument } from "./testing/fakeDocument";

const TEXT = "\\begin{document}\nLet $x+1$ be.\n\\[\\mycmd{y}=2\\]\n\\end{document}";

function setup(text = TEXT, path = "main.tex") {
  const doc = fakeDocument(text, { path });
  let next: Resolved = { error: "no caret" };
  const store = new EquationPanelStore({
    resolve: async () => next,
    document: (id) => (id === doc.id && !doc.closed ? doc : undefined),
    showPanel: () => {},
    copy: async () => true,
  });
  const at = (start: number, end = start): CaretTarget => {
    next = { surface: "source", document: doc, path, start, end, focus: vi.fn() };
    return next;
  };
  const onEditInSource = vi.fn();
  const view = (activePath: string | null = path) =>
    render(<EquationView store={store} activePath={activePath} onEditInSource={onEditInSource} />);
  return { doc, store, at, view, onEditInSource };
}

const editor = () => screen.getByTestId("equation-editor");
const key = (k: string, init: Partial<KeyboardEventInit> = {}) => fireEvent.keyDown(editor(), { key: k, ...init });
const children = (s: EquationPanelStore) => s.getState().draft!.editor.root.children;

async function openAt(s: ReturnType<typeof setup>, offset: number, mode: "edit" | "inline" = "edit") {
  s.at(offset);
  await act(async () => {
    await s.store.open(mode);
  });
}

afterEach(cleanup);

describe("Equation panel", () => {
  it("offers insertion and the symbol browser when no draft is open", () => {
    const s = setup();
    s.view();
    expect(screen.getByText("No equation open")).toBeTruthy();
    expect(screen.getByTestId("equation-insert-inline")).toBeTruthy();
    expect(screen.getByTestId("equation-symbol-grid")).toBeTruthy();
  });

  it("says what to open for a document without equation support", () => {
    const s = setup();
    s.view("notes.txt");
    expect(screen.getByText(/Open a LaTeX, Typst, or Markdown document/)).toBeTruthy();
    expect(screen.queryByTestId("equation-symbol-grid")).toBeNull();
  });

  it("identifies the target and takes focus when a draft opens", async () => {
    const s = setup();
    s.view();
    await openAt(s, TEXT.indexOf("x+1"));
    expect(screen.getByTestId("equation-target").textContent).toContain("Editing equation at line 2");
    expect(screen.getByTestId("equation-target").textContent).toContain("main.tex · LaTeX · from the source · inline");
    expect(document.activeElement).toBe(editor());
  });

  it("edits with the keyboard: typing, arrows, Backspace, Tab between slots", async () => {
    const s = setup();
    s.view();
    await openAt(s, TEXT.indexOf("."), "inline");
    key("a");
    key("/");
    key("b");
    expect(children(s.store)).toEqual([{ kind: "frac", num: { kind: "row", children: [{ kind: "ident", text: "a" }] }, den: { kind: "row", children: [{ kind: "ident", text: "b" }] } }]);
    // Shift+Tab goes back to the numerator; Tab returns to the denominator.
    key("Tab", { shiftKey: true });
    expect(s.store.getState().draft!.editor.caret.path).toEqual([0, "num"]);
    key("Tab");
    expect(s.store.getState().draft!.editor.caret.path).toEqual([0, "den"]);
    key("End");
    key("ArrowLeft", { shiftKey: true });
    expect(s.store.getState().draft!.editor.caret).toEqual({ path: [0, "den"], offset: 0, anchor: 1 });
    expect(document.querySelector('.xp-eq-item[data-selected]')?.textContent).toBe("b");
    key("Backspace");
    expect((children(s.store)[0] as { den: { children: unknown[] } }).den.children).toHaveLength(0);
    // The empty slot is visible.
    expect(document.querySelector('.xp-eq-row[data-slot="den"][data-empty]')).toBeTruthy();
    // Tab from the last slot leaves the structure, then the editor (not trapped).
    key("Tab");
    expect(s.store.getState().draft!.editor.caret.path).toEqual([]);
    expect(key("Tab")).toBe(true); // default not prevented: focus moves on
  });

  it("builds a nested fraction with a root and scripts from templates and typing", async () => {
    const s = setup();
    s.view();
    await openAt(s, TEXT.indexOf("."), "inline");
    fireEvent.click(screen.getByRole("button", { name: "Fraction" }));
    key("1");
    key("Tab");
    fireEvent.click(screen.getByRole("button", { name: "Square root" }));
    key("x");
    key("^");
    key("2");
    const frac = children(s.store)[0] as { kind: string; den: { children: { kind: string }[] } };
    expect(frac.kind).toBe("frac");
    expect(frac.den.children[0]!.kind).toBe("sqrt");
    expect(screen.getByTestId("equation-preview").textContent).not.toContain("No preview");
    await act(async () => {
      key("Enter");
    });
    expect(s.doc.model.getValue()).toContain("Let $x+1$ be$\\frac{1}{\\sqrt{x^{2}}}$.");
  });

  it("Enter applies only when unambiguous; Ctrl+Enter always applies; Escape cancels", async () => {
    const s = setup();
    s.view();
    await openAt(s, TEXT.indexOf("."), "inline");
    act(() => s.store.edit((st) => insertTemplate(st, "matrix", { rows: 2, cols: 2 })));
    key("a");
    key("Enter");
    expect(s.store.getState().draft).not.toBeNull();
    expect(screen.getByTestId("equation-grid-actions")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Row below" }));
    expect((children(s.store)[0] as { rows: unknown[] }).rows).toHaveLength(3);
    await act(async () => {
      key("Enter", { ctrlKey: true });
    });
    expect(s.store.getState().draft).toBeNull();
    const written = s.doc.model.getValue();
    expect(written).toContain("be$\\begin{pmatrix}a &");
    expect(written.split("\\\\").length - 1).toBe(2); // three rows

    await openAt(s, TEXT.indexOf("x+1"));
    key("z");
    key("Escape");
    expect(s.store.getState().draft).toBeNull();
    expect(s.doc.model.getValue()).toContain("Let $x+1$ be");
  });

  it("finds symbols by name, synonym, glyph, and command, with an empty state", async () => {
    const s = setup();
    s.view();
    const search = screen.getByRole("searchbox", { name: "Search symbols" });
    const firstOption = () => within(screen.getByTestId("equation-symbol-grid")).getAllByRole("option")[0]!;
    for (const query of ["less than or equal", "leq", "≤", "<="]) {
      fireEvent.change(search, { target: { value: query } });
      await waitFor(() => expect(firstOption().getAttribute("data-symbol")).toBe("rel.leq"));
    }
    expect(screen.getByTestId("equation-symbol-detail").textContent).toContain("less than or equal to · \\leq");
    fireEvent.change(search, { target: { value: "zzzz" } });
    await waitFor(() => expect(screen.getByText("No symbols match “zzzz”")).toBeTruthy());
  });

  it("inserts with the mouse without taking focus from the equation, at the retained caret", async () => {
    const s = setup();
    s.view();
    await openAt(s, TEXT.indexOf("x+1"));
    key("Home");
    editor().focus();
    const search = screen.getByRole("searchbox", { name: "Search symbols" });
    fireEvent.change(search, { target: { value: "alpha" } });
    editor().focus();
    const option = await screen.findByRole("option", { name: "alpha" });
    expect(fireEvent.mouseDown(option)).toBe(false); // default prevented: focus stays
    fireEvent.click(option);
    expect(document.activeElement).toBe(editor());
    expect(children(s.store)[0]).toEqual({ kind: "symbol", id: "greek.alpha" });
  });

  it("navigates the symbol grid with the keyboard and refuses unavailable symbols", async () => {
    const s = setup();
    s.view();
    await openAt(s, TEXT.indexOf("x+1"));
    const grid = screen.getByTestId("equation-symbol-grid");
    grid.focus();
    fireEvent.keyDown(grid, { key: "ArrowRight" });
    expect(grid.getAttribute("aria-activedescendant")).toBe(within(grid).getAllByRole("option")[1]!.id);
    fireEvent.keyDown(grid, { key: "Enter" });
    expect(children(s.store).at(-1)).toEqual({ kind: "symbol", id: "greek.beta" });
    expect(document.activeElement).toBe(grid);
    const count = children(s.store).length;
    const unavailable = within(grid).getByRole("option", { name: "typst-only mark, not available in LaTeX" });
    expect(unavailable.getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(unavailable);
    expect(children(s.store)).toHaveLength(count);
  });

  it("shows a requirement with an explicit option, applied in the same edit", async () => {
    const s = setup();
    s.view();
    await openAt(s, TEXT.indexOf("x+1"));
    act(() => s.store.edit((st) => insertSymbol(st, "rel.lesssim")));
    const banner = screen.getByTestId("equation-requirement");
    expect(banner.textContent).toContain("Needs \\usepackage{amssymb}");
    const checkbox = within(banner).getByRole("checkbox");
    expect((checkbox as HTMLInputElement).checked).toBe(true);
    fireEvent.click(checkbox);
    expect(s.store.getState().draft!.addRequirements).toBe(false);
  });

  it("keeps unsupported source and hands off to the source editor", async () => {
    const s = setup();
    s.view();
    await openAt(s, TEXT.indexOf("=2"));
    const banner = screen.getByTestId("equation-opaque");
    expect(banner.textContent).toContain("Unknown command \\mycmd");
    expect(screen.getByText("\\mycmd{y}")).toBeTruthy();
    fireEvent.click(within(banner).getByRole("button", { name: "Edit in Source" }));
    const start = TEXT.indexOf("\\[");
    expect(s.onEditInSource).toHaveBeenCalledWith("main.tex", { start, end: TEXT.indexOf("\\]") + 2 });
  });

  it("shows a conflict when the target changes, disables Apply, and offers recovery", async () => {
    const s = setup();
    s.view();
    await openAt(s, TEXT.indexOf("x+1"));
    key("q");
    act(() => s.doc.model.edit([{ rangeOffset: TEXT.indexOf("x+1"), rangeLength: 1, text: "w" }]));
    const banner = screen.getByTestId("equation-conflict");
    expect(banner.textContent).toContain("Nothing was written; the draft is kept.");
    expect(within(banner).getByRole("button", { name: "Use current equation" })).toBeTruthy();
    expect(screen.getByTestId("equation-apply").hasAttribute("disabled") || screen.getByTestId("equation-apply").getAttribute("data-disabled") === "true").toBe(true);
    fireEvent.click(within(banner).getByRole("button", { name: "Use current equation" }));
    expect(screen.queryByTestId("equation-conflict")).toBeNull();
  });

  it("keeps a draft for its own document when another document is active", async () => {
    const s = setup();
    const { rerender } = s.view();
    await openAt(s, TEXT.indexOf("x+1"));
    rerender(<EquationView store={s.store} activePath="other.tex" onEditInSource={s.onEditInSource} />);
    const banner = screen.getByTestId("equation-other-document");
    expect(banner.textContent).toContain("This draft belongs to main.tex");
    expect(within(banner).getByRole("button", { name: "Move to caret" })).toBeTruthy();
  });

  it("places the caret in a slot with the mouse", async () => {
    const s = setup();
    s.view();
    await openAt(s, TEXT.indexOf("."), "inline");
    fireEvent.click(screen.getByRole("button", { name: "Fraction" }));
    expect(s.store.getState().draft!.editor.caret.path).toEqual([0, "num"]);
    fireEvent.mouseDown(document.querySelector('.xp-eq-row[data-slot="den"]')!, { button: 0 });
    expect(s.store.getState().draft!.editor.caret).toEqual({ path: [0, "den"], offset: 0 });
  });

  it("shows a closed document as a conflict before Apply is tried", async () => {
    const s = setup();
    const { rerender } = s.view();
    await openAt(s, TEXT.indexOf("x+1"));
    s.doc.closed = true;
    s.doc.model.dispose();
    rerender(<EquationView store={s.store} activePath={null} onEditInSource={s.onEditInSource} />);
    expect(screen.getByTestId("equation-conflict").textContent).toContain("main.tex was closed");
    expect(screen.getByTestId("equation-view").getAttribute("data-conflict")).toBe("closed");
  });

  it("toggles inline/display for a new equation in text", async () => {
    const s = setup();
    s.view();
    await openAt(s, TEXT.indexOf("."), "inline");
    expect(screen.getByTestId("equation-target").textContent).toContain("New inline equation at line 2");
    fireEvent.click(screen.getByRole("radio", { name: "Display" }));
    expect(s.store.getState().draft!.display).toBe(true);
    expect(screen.getByTestId("equation-target").textContent).toContain("New display equation at line 2");
  });
});
