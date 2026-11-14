// The edge component's label behavior (React Flow draws edges only once node
// handles are measured, which jsdom cannot do; the node lookup, BaseEdge, and
// the label portal are stubbed so the real RelationEdge logic runs).
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { EdgeProps } from "@xyflow/react";
import type { ReactNode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("@xyflow/react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@xyflow/react")>();
  return {
    ...actual,
    useInternalNode: (id: string) => ({
      id,
      internals: { positionAbsolute: { x: id === "lem" ? 400 : 0, y: 0 } },
      measured: { width: 208, height: 96 },
    }),
    BaseEdge: ({ id, path }: { id: string; path: string }) => <span data-testid={`path-${id}`} data-d={path} />,
    EdgeLabelRenderer: ({ children }: { children: ReactNode }) => <>{children}</>,
  };
});

const { CanvasContext } = await import("./context");
const { createInterestStore } = await import("./interaction");
const { EdgeLabelContent, RelationEdge } = await import("./RelationEdge");
type RelationRFEdge = import("./RelationEdge").RelationRFEdge;
type CanvasEdge = import("./types").CanvasEdge;

afterEach(cleanup);

function renderEdge(
  edge: Pick<CanvasEdge, "label" | "family" | "status"> & { count?: number; selected?: boolean },
  options: { labels?: "all" | "selection"; selectedNodes?: string[] } = {},
) {
  const interest = createInterestStore();
  const onSelectEdge = vi.fn();
  const props = {
    id: "r1",
    source: "thm",
    target: "lem",
    selected: edge.selected ?? false,
    data: {
      kind: "dependsOn",
      label: edge.label,
      family: edge.family,
      status: edge.status,
      count: edge.count,
      description: `Main theorem ${edge.label} Key lemma`,
      bow: 0,
      forward: true,
    },
  } as unknown as EdgeProps<RelationRFEdge>;
  render(
    <CanvasContext.Provider
      value={{
        markerPrefix: "t",
        connectable: false,
        labels: options.labels ?? "all",
        selectedNodes: new Set(options.selectedNodes ?? []),
        interest,
        onSelectEdge,
      }}
    >
      <RelationEdge {...props} />
    </CanvasContext.Provider>,
  );
  return { interest, onSelectEdge, label: () => document.querySelector<HTMLElement>(".xp-graph-edge-label") };
}

describe("RelationEdge labels", () => {
  it("reads an accepted relationship plainly, and a suggested or observed one with a muted status suffix (no uppercase prefix)", () => {
    const accepted = renderEdge({ label: "depends on", family: "math", status: "accepted" });
    expect(accepted.label()!.textContent).toBe("depends on");
    expect(accepted.label()!.querySelector(".xp-graph-edge-label__status")).toBeNull();
    cleanup();

    const suggested = renderEdge({ label: "depends on", family: "math", status: "suggested" });
    expect(suggested.label()!.textContent).toBe("depends on· suggested");
    expect(suggested.label()!.querySelector(".xp-graph-edge-label__reading")!.textContent).toBe("depends on");
    expect(suggested.label()!.querySelector(".xp-graph-edge-label__status")!.textContent).toBe("· suggested");
    expect(suggested.label()!.getAttribute("data-status")).toBe("suggested");
    expect(suggested.label()!.textContent).not.toMatch(/SUGGESTED|Suggested/);
    // The accessible name stays the relationship's description.
    expect(screen.getByRole("button", { name: "Main theorem depends on Key lemma" })).toBe(suggested.label());
    cleanup();

    const observed = renderEdge({ label: "references", family: "citation", status: "observed", count: 3 });
    expect(observed.label()!.querySelector(".xp-graph-edge-label__count")!.textContent).toBe("×3");
    expect(observed.label()!.querySelector(".xp-graph-edge-label__status")!.textContent).toBe("· observed");
  });

  it("shows a structure relationship's label only while it or one of its nodes has interest", () => {
    const { interest, label } = renderEdge({ label: "is declared in", family: "structure", status: "observed" });
    expect(label()).toBeNull();
    act(() => interest.setState({ hovered: { kind: "node", id: "lem" } }));
    expect(label()!.textContent).toBe("is declared in· observed");
    act(() => interest.setState({ hovered: { kind: "node", id: "elsewhere" } }));
    expect(label()).toBeNull();
    act(() => interest.setState({ focused: { kind: "edge", id: "r1" } }));
    expect(label()).not.toBeNull();
    act(() => interest.setState({ focused: null }));
    expect(label()).toBeNull();
  });

  it("shows a structure relationship's label when it or one of its nodes is selected", () => {
    const viaNode = renderEdge({ label: "contains", family: "structure", status: "accepted" }, { selectedNodes: ["thm"] });
    expect(viaNode.label()!.textContent).toBe("contains");
    cleanup();
    const viaEdge = renderEdge({ label: "contains", family: "structure", status: "accepted", selected: true });
    expect(viaEdge.label()!.getAttribute("data-selected")).toBe("true");
  });

  it("keeps a hovered label visible while the pointer is on it, and selects the edge on click", () => {
    const { interest, label, onSelectEdge } = renderEdge({ label: "imports", family: "resource", status: "accepted" });
    act(() => interest.setState({ hovered: { kind: "edge", id: "r1" } }));
    const chip = label()!;
    // Moving from the edge path onto its label keeps the edge as the hovered item.
    fireEvent.mouseEnter(chip);
    expect(interest.getState().hovered).toEqual({ kind: "edge", id: "r1" });
    fireEvent.click(chip);
    expect(onSelectEdge).toHaveBeenCalledWith("r1");
    fireEvent.mouseLeave(chip);
    expect(interest.getState().hovered).toBeNull();
    expect(label()).toBeNull();
  });

  it("hides a mathematical label above 60 edges unless the selection is next to it", () => {
    const far = renderEdge({ label: "depends on", family: "math", status: "accepted" }, { labels: "selection" });
    expect(far.label()).toBeNull();
    cleanup();
    const near = renderEdge({ label: "depends on", family: "math", status: "accepted" }, { labels: "selection", selectedNodes: ["lem"] });
    expect(near.label()).not.toBeNull();
  });
});

describe("EdgeLabelContent", () => {
  it("orders reading, folded count, then status", () => {
    render(
      <span data-testid="chip">
        <EdgeLabelContent label="depends on" count={2} status="suggested" />
      </span>,
    );
    expect(screen.getByTestId("chip").textContent).toBe("depends on×2· suggested");
  });
});
