// XR-B-01: a Problems click on a page view whose pages cannot show the
// diagnostic (no pages, or source added after the last good build) shows it
// in the source beside the pages; a position the pages can show stays a page
// reveal.
import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { BuildEvent } from "../../bindings/BuildEvent";
import type { CaretGeometry } from "../../bindings/CaretGeometry";

const ipc = vi.hoisted(() => ({ caret: null as unknown }));

vi.mock("../../lib/ipc", () => ({
  commands: {
    documentState: async () => ({ info: { revision: 1 }, text: "Hello" }),
    typesettingWatch: async () => null,
    typesettingUnwatch: async () => {},
    pageCaret: async () => ipc.caret,
    pageImage: async () => [],
  },
  events: {
    documents: async () => () => {},
    typesettingBuild: async () => () => {},
    trustChanged: async () => () => {},
  },
  errorMessage: (e: unknown) => String(e),
  isCommandError: () => false,
  newId: (prefix: string) => `${prefix}-1`,
}));

const ui = vi.hoisted(() => ({
  /** The PageEditor stub's latest props. */
  page: null as null | { onRevealMissed?: (offset: number) => void },
  editor: null as null | Record<string, ReturnType<typeof vi.fn>>,
}));

vi.mock("../editor", () => ({
  openDocument: async () => ({ id: "doc-1", editReason: null, subscribe: () => () => {} }),
  fileKey: (path: string) => `file:${path}`,
  SourceEditor: ({ onEditor }: { onEditor?: (editor: unknown, handle: unknown) => void }) => {
    // Mounts like the real one: the Monaco editor exists after an effect.
    useEffectOnce(() => onEditor?.(ui.editor, { path: "paper.typ" }));
    return <div data-testid="source-stub" />;
  },
}));
vi.mock("../jobs/trust", () => ({ requestWorkspaceTrust: vi.fn() }));
// jsdom has no layout: the split's panels render their children only.
vi.mock("react-resizable-panels", () => ({
  Group: ({ children }: { children: unknown }) => <div>{children as never}</div>,
  Panel: ({ children }: { children: unknown }) => <div>{children as never}</div>,
}));
vi.mock("../../ui/components", () => ({ ResizeHandle: () => null }));
vi.mock("./PageEditor", () => ({
  PageEditor: (props: { onRevealMissed?: (offset: number) => void }) => {
    ui.page = props;
    return <div data-testid="page-stub" />;
  },
}));

import { useEffect } from "react";

function useEffectOnce(effect: () => void) {
  // eslint-disable-next-line react-hooks/exhaustive-deps
  useEffect(effect, []);
}

const { PageSession } = await import("./session");
const { TypstEditor } = await import("./TypstEditor");
const { editors } = await import("../../app/editors");
const { requestReveal, useTypesetting } = await import("../typesetting/store");

const GEOMETRY = { page: 0, x: 10, top: 10, bottom: 20, mode: "text" } as unknown as CaretGeometry;

function succeeded(): BuildEvent {
  return {
    result: {
      id: "typst-1",
      target: { format: "typst", engine: "typst", entry: "paper.typ" },
      snapshot: "",
      sources: [{ document: "doc-1", revision: 1 }],
      configuration: null,
      status: { kind: "succeeded" },
      diagnostics: [],
      pages: [],
      mapping: "none",
      exports: [],
      durationMs: 0,
    },
    timings: { snapshotMs: 0, compileMs: 0, mapMs: 0, totalMs: 0 },
    lastGood: null,
    refusal: null,
  } as unknown as BuildEvent;
}

describe("page session reveal", () => {
  afterEach(() => {
    ipc.caret = null;
  });

  it("says whether the shown pages can show the position", async () => {
    const session = new PageSession("paper.typ");
    await session.attach();
    // No pages yet: nothing to show it on.
    expect(await session.reveal(3)).toBe(false);
    await session.onBuild(succeeded());
    ipc.caret = GEOMETRY;
    expect(await session.reveal(3)).toBe(true);
    expect(session.getModel().caret).toEqual(GEOMETRY);
    // A position the pages do not contain (e.g. appended after the build).
    ipc.caret = null;
    expect(await session.reveal(400)).toBe(false);
    session.detach();
  });
});

describe("Problems reveal in a page view", () => {
  const resource = { key: "file:paper.typ", path: "paper.typ", title: "paper.typ" };

  beforeEach(() => {
    useTypesetting.setState({ entries: {}, reveal: null });
    ui.editor = {
      getModel: vi.fn(() => ({ getPositionAt: (offset: number) => ({ lineNumber: 21, column: offset }) })),
      setPosition: vi.fn(),
      revealPositionInCenter: vi.fn(),
      focus: vi.fn(),
    };
  });
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it("switches to Split and places the source caret when the pages cannot show it", async () => {
    const setMode = vi.spyOn(editors, "setMode").mockImplementation(() => {});
    const view = render(<TypstEditor resource={resource} mode="page" active />);
    act(() => requestReveal("paper.typ", 15));
    act(() => ui.page?.onRevealMissed?.(15));
    expect(setMode).toHaveBeenCalledWith("file:paper.typ", "split");
    // The workbench applies the mode: the source editor mounts and shows the position.
    view.rerender(<TypstEditor resource={resource} mode="split" active />);
    expect(ui.editor?.setPosition).toHaveBeenCalledWith({ lineNumber: 21, column: 15 });
    expect(ui.editor?.revealPositionInCenter).toHaveBeenCalled();
    expect(ui.editor?.focus).toHaveBeenCalled();
  });

  it("stays on the page when the page shows the position", () => {
    const setMode = vi.spyOn(editors, "setMode").mockImplementation(() => {});
    render(<TypstEditor resource={resource} mode="page" active />);
    act(() => requestReveal("paper.typ", 15));
    // The page view placed its caret (no miss reported).
    expect(setMode).not.toHaveBeenCalled();
  });

  it("does not switch away from a mode the person chose for a miss reported later", () => {
    const setMode = vi.spyOn(editors, "setMode").mockImplementation(() => {});
    const view = render(<TypstEditor resource={resource} mode="split" active />);
    act(() => requestReveal("paper.typ", 15));
    // In Split the source shows it already; a page miss changes nothing.
    view.rerender(<TypstEditor resource={resource} mode="split" active />);
    act(() => ui.page?.onRevealMissed?.(15));
    expect(setMode).not.toHaveBeenCalled();
  });
});
