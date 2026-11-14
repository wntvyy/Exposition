import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import { History } from "lucide-react";

import { GraphCanvas, toFlowEdges, toFlowNode, type CanvasSelection } from "./GraphCanvas";
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
beforeEach(() => localStorage.clear());

const nodes: CanvasNode[] = [
  { id: "thm", title: "Main theorem", typeLabel: "Theorem", category: "math", badges: [{ label: "Stale", tone: "stale", icon: History }], position: { x: 0, y: 0 } },
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
    // Status badges keep their icon next to the text.
    const badge = node.querySelector(".ui-badge")!;
    expect(badge.textContent).toBe("Stale");
    expect(badge.querySelector("svg")).not.toBeNull();
  });

  it("offers a keyboard-reachable legend for line color (family) and line style (status)", () => {
    renderCanvas();
    const controls = screen.getByRole("toolbar", { name: "Canvas view controls" });
    const toggle = within(controls).getByRole("button", { name: "Legend" });
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByRole("region", { name: "Legend" })).toBeNull();

    act(() => toggle.focus());
    fireEvent.click(toggle);
    const legend = screen.getByRole("region", { name: "Legend" });
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    expect(toggle.getAttribute("aria-controls")).toBe(legend.id);
    const rows = [...legend.querySelectorAll("li")].map((li) => li.textContent);
    expect(rows).toEqual([
      "Mathematical dependenciesdepends on, assumes, is a proof of",
      "References",
      "Evidence and provenance",
      "Formal association",
      "Structure and resource usecontains, is declared in, imports",
      "Acceptedsolid",
      "Suggesteddashed",
      "Observeddotted",
    ]);
    // Samples use the edges' own stroke rules and this canvas's arrowheads.
    const math = legend.querySelector('.xp-graph-legend__line[data-family="math"]')!;
    const marker = /url\(#(.+)\)/.exec(math.getAttribute("marker-end") ?? "")?.[1];
    expect(marker && document.getElementById(marker)).toBeTruthy();
    expect(legend.querySelector('.xp-graph-legend__line[data-status="suggested"]')).not.toBeNull();
    expect(legend.querySelector('.xp-graph-legend__line[data-family="citation"]')!.getAttribute("marker-end")).toMatch(/-citation-open\)$/);
    // The legend never counts as an edge.
    expect(document.querySelectorAll(".xp-edge").length).toBe(0);

    fireEvent.click(toggle);
    expect(screen.queryByRole("region", { name: "Legend" })).toBeNull();
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
  });

  it("remembers whether the legend is open", () => {
    renderCanvas();
    fireEvent.click(screen.getByRole("button", { name: "Legend" }));
    cleanup();
    renderCanvas();
    expect(screen.getByRole("region", { name: "Legend" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Legend" }).getAttribute("aria-expanded")).toBe("true");
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

describe("toFlowNode", () => {
  it("names an inactive item's lifecycle once, through its badge", () => {
    const archived: CanvasNode = { id: "def", title: "Compactness", typeLabel: "Definition", category: "math", badges: [{ label: "Archived", tone: "neutral", dashed: true }], inactive: true, position: { x: 0, y: 0 } };
    expect(toFlowNode(archived, undefined, false).ariaLabel).toBe("Definition, Compactness, Archived");
    const removed = { ...archived, badges: [{ label: "Removed from source", tone: "warning" as const, dashed: true }] };
    expect(toFlowNode(removed, undefined, false).ariaLabel).toBe("Definition, Compactness, Removed from source");
    expect(toFlowNode({ ...archived, badges: [] }, undefined, false).ariaLabel).toBe("Definition, Compactness, inactive");
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
