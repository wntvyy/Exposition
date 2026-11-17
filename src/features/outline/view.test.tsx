// The Outline view: rows in source order with the unsaved-buffer label and
// coverage, navigation on click, the compiled pages of the last good build
// labelled after a failed build, and an honest state for other files.
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { BuildEvent } from "../../bindings/BuildEvent";
import type { DocumentOutline } from "../../bindings/DocumentOutline";

const env = vi.hoisted(() => ({
  active: null as unknown,
  outline: null as unknown,
  entries: {} as Record<string, unknown>,
  navigate: vi.fn(async () => "shown"),
  showPage: vi.fn(async () => {}),
}));

const handle = { id: "doc-1", closed: false, sync: { revision: 7 }, subscribe: () => () => {}, getVersion: () => 1 };

vi.mock("../workspace/store", () => ({
  useWorkspace: (select: (s: { info: unknown }) => unknown) => select({ info: { root: "C:/p", name: "p", recovery: [] } }),
}));
vi.mock("../../app/editors", () => ({ useActiveEditor: () => env.active }));
vi.mock("../../app/registry", () => ({ registry: { editorFor: () => ({ modes: ["source", "page", "split"] }) } }));
vi.mock("../editor", () => ({ openDocument: vi.fn(async () => handle) }));
vi.mock("../../lib/ipc", () => ({ commands: { documentOutline: vi.fn(async () => env.outline) }, errorMessage: String }));
vi.mock("./navigate", () => ({ navigateToItem: env.navigate, showPage: env.showPage }));
vi.mock("../typesetting/store", async (original) => {
  const real = await original<typeof import("../typesetting/store")>();
  return { ...real, useTypesetting: (select: (s: { entries: unknown }) => unknown) => select({ entries: env.entries }) };
});

const { OutlineView } = await import("./OutlineView");

const item = (id: string, kind: "heading" | "declaration", typeLabel: string, title: string, children: unknown[] = []) => ({
  id,
  kind,
  entityKind: kind === "heading" ? null : "theorem",
  syntax: id.split("|")[0],
  typeLabel,
  title,
  label: kind === "heading" ? null : "thm:main",
  level: kind === "heading" ? 1 : 0,
  range: { start: 0, end: 10 },
  selection: { start: 0, end: 5 },
  header: "x",
  children,
});

const OUTLINE: DocumentOutline = {
  path: "main.typ",
  revision: 7,
  unsaved: true,
  extractor: "typst/1",
  coverage: "partial",
  detail: "Not interpreted: headings written as #heading(…) calls are not listed",
  items: [item("typst.heading|Intro|0", "heading", "Heading 1", "Intro", [item("typst.call:theorem|Main|0", "declaration", "Theorem", "Main")])],
  itemCount: 2,
} as DocumentOutline;

function buildEvent(status: "succeeded" | "failed", revision: number, pages: number, lastGood: unknown = null): BuildEvent {
  return {
    result: {
      id: `b${revision}`,
      status: { kind: status },
      sources: [{ path: "main.typ", document: "doc-1", revision, contentHash: "" }],
      pages: Array.from({ length: pages }, (_, index) => ({ index, widthPt: 1, heightPt: 1, renderRef: `r${index}` })),
    },
    lastGood,
    refusal: null,
  } as unknown as BuildEvent;
}

beforeEach(() => {
  env.active = { resource: { key: "file:main.typ", path: "main.typ", title: "main.typ" }, mode: "source" };
  env.outline = OUTLINE;
  env.entries = {};
  vi.clearAllMocks();
});

afterEach(() => cleanup());

describe("Outline view", () => {
  it("lists the structure of the unsaved buffer, says coverage, and navigates", async () => {
    render(<OutlineView />);
    const tree = await screen.findByTestId("outline-tree", {}, { timeout: 3000 });
    const rows = within(tree).getAllByRole("row").map((r) => r.querySelector(".ui-tree-item__title")?.textContent);
    expect(rows).toEqual(["Intro", "Theorem (Main)"]);
    expect(screen.getByTestId("outline-unsaved").textContent).toBe("Unsaved buffer (revision 7)");
    expect(screen.getByTestId("outline-coverage").textContent).toMatch(/^Extraction: Partial \(typst\/1\) — Not interpreted: headings written as #heading/);
    fireEvent.click(within(tree).getAllByRole("row")[1]!);
    await waitFor(() => expect(env.navigate).toHaveBeenCalledWith(expect.objectContaining({ id: "typst.call:theorem|Main|0" }), OUTLINE, expect.anything()));
  });

  it("keeps the last good build's pages, labelled, after a failed build", async () => {
    env.entries = { "main.typ": { entry: "main.typ", activity: "idle", latest: buildEvent("failed", 7, 0, buildEvent("succeeded", 5, 2).result) } };
    render(<OutlineView />);
    const pages = await screen.findByTestId("outline-pages", {}, { timeout: 3000 });
    expect(pages.getAttribute("data-state")).toBe("failed");
    expect(pages.getAttribute("data-build")).toBe("b5");
    expect(within(pages).getByText("Showing pages from an earlier revision")).toBeTruthy();
    const list = within(pages).getByTestId("outline-page-list");
    expect(within(list).getAllByRole("row").map((r) => r.textContent)).toEqual(["Page 1", "Page 2"]);
    fireEvent.click(within(list).getAllByRole("row")[1]!);
    await waitFor(() => expect(env.showPage).toHaveBeenCalledWith("main.typ", 1));
  });

  it("offers the page view when no pages were built", async () => {
    render(<OutlineView />);
    const pages = await screen.findByTestId("outline-pages", {}, { timeout: 3000 });
    expect(pages.getAttribute("data-state")).toBe("none");
    fireEvent.click(within(pages).getByRole("button", { name: "Show Page view" }));
    expect(env.showPage).toHaveBeenCalledWith("main.typ", 0);
  });

  it("says when a file has no outline, without reading it", () => {
    env.active = { resource: { key: "file:data.csv", path: "data.csv", title: "data.csv" }, mode: "source" };
    render(<OutlineView />);
    expect(screen.getByText("No outline for this kind of file")).toBeTruthy();
  });

  it("asks for a document when none is active", () => {
    env.active = undefined;
    render(<OutlineView />);
    expect(screen.getByText("No document active")).toBeTruthy();
  });
});
