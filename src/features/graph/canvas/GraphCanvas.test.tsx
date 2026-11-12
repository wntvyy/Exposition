import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { GraphCanvas, toFlowEdges, type CanvasSelection } from "./GraphCanvas";
import type { CanvasEdge, CanvasNode } from "./types";

beforeAll(() => {
  globalThis.ResizeObserver ??= class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
  // React Flow reads the viewport transform through DOMMatrixReadOnly.
  (globalThis as { DOMMatrixReadOnly?: unknown }).DOMMatrixReadOnly ??= class {
    m22 = 1;
    constructor(transform?: string) {
      const scale = /scale\(([\d.]+)\)/.exec(transform ?? "");
      this.m22 = scale ? Number(scale[1]) : 1;
    }
  };
  globalThis.matchMedia ??= ((query: string) => ({
    matches: false,
    media: query,
    addEventListener() {},
    removeEventListener() {},
  })) as unknown as typeof matchMedia;
});

afterEach(cleanup);

const nodes: CanvasNode[] = [
  { id: "thm", title: "Main theorem", typeLabel: "Theorem", category: "math", badges: [{ label: "Stale", tone: "stale" }], position: { x: 0, y: 0 } },
  { id: "lem", title: "Key lemma", typeLabel: "Lemma", category: "math", badges: [], position: { x: 300, y: 0 } },
  { id: "src", title: "paper.tex", typeLabel: "Manuscript", category: "document", badges: [], position: { x: 0, y: 200 } },
];
const edges: CanvasEdge[] = [
  { id: "r1", source: "thm", target: "lem", kind: "depends_on", label: "depends on", family: "math", status: "accepted", description: "Main theorem depends on Key lemma" },
  { id: "r2", source: "thm", target: "lem", kind: "references", label: "references", family: "citation", status: "observed", description: "Main theorem references Key lemma" },
];

function renderCanvas(overrides: Partial<Parameters<typeof GraphCanvas>[0]> = {}) {
  const props = {
    "aria-label": "Project graph",
    nodes,
    edges,
    viewport: { x: 0, y: 0, zoom: 1 },
    selection: { nodes: [], edge: null } as CanvasSelection,
    onSelectionChange: vi.fn(),
    onNodesMoved: vi.fn(),
    onViewportChange: vi.fn(),
    ...overrides,
  };
  render(<GraphCanvas {...props} />);
  return props;
}

describe("GraphCanvas", () => {
  it("renders each entity as a named rectangle with its type and badges", () => {
    renderCanvas();
    const canvas = screen.getByRole("group", { name: "Project graph" });
    expect(canvas.getAttribute("aria-roledescription")).toBe("canvas");
    const node = canvas.querySelector('[data-id="thm"]')!;
    expect(node.getAttribute("aria-label")).toBe("Theorem, Main theorem, Stale");
    expect(node.querySelector(".ui-node-card")?.getAttribute("data-category")).toBe("math");
    expect(screen.getByText("Key lemma")).toBeTruthy();
  });

  it("never deletes on Delete or Backspace", () => {
    const props = renderCanvas({ selection: { nodes: ["thm"], edge: null } });
    const node = document.querySelector('[data-id="thm"]') as HTMLElement;
    act(() => node.focus());
    fireEvent.keyDown(node, { key: "Delete", code: "Delete" });
    fireEvent.keyDown(node, { key: "Backspace", code: "Backspace" });
    expect(document.querySelector('[data-id="thm"]')).not.toBeNull();
    expect(props.onNodesMoved).not.toHaveBeenCalled();
  });

  it("selects a node on click and reports it once", async () => {
    const props = renderCanvas();
    const node = document.querySelector('[data-id="lem"]') as HTMLElement;
    fireEvent.click(node);
    await act(async () => {});
    expect(props.onSelectionChange).toHaveBeenCalledWith({ nodes: ["lem"], edge: null });
  });

  it("shows the connection handle only when relationships may be drawn", () => {
    renderCanvas();
    expect(document.querySelector(".xp-graph-node__source.connectable")).toBeNull();
    cleanup();
    renderCanvas({ onConnect: vi.fn() });
    expect(document.querySelector(".xp-graph-node__source.connectable")).not.toBeNull();
  });
});

describe("toFlowEdges", () => {
  it("gives parallel relations opposite bows and keeps the accessible description", () => {
    const flow = toFlowEdges(edges, "r2");
    expect(flow.map((e) => e.data!.bow)).toEqual([-11, 11]);
    expect(flow[1]!.selected).toBe(true);
    expect(flow[0]!.ariaLabel).toBe("Main theorem depends on Key lemma");
    expect(flow[0]!.className).toContain("xp-edge--math");
    expect(flow[1]!.className).toContain("xp-edge--observed");
  });
});
