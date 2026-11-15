// Research › Workflows inspector: "Open in the workflow editor" opens the
// workflow by its own id (wf-…), never by the graph entity id the Research
// listing selects it with (the 2026-11-15 incident: `workflow:ent-…` reached
// `workflow_get`, which refuses anything that is not a workflow id).
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ResearchListing } from "../../bindings/ResearchListing";

vi.hoisted(() => {
  globalThis.matchMedia ??= ((query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addEventListener() {},
    removeEventListener() {},
    addListener() {},
    removeListener() {},
    dispatchEvent: () => false,
  })) as unknown as typeof matchMedia;
});

const graph = vi.hoisted(() => ({ entity: vi.fn(() => new Promise(() => {})), impact: vi.fn(() => new Promise(() => {})) }));
vi.mock("../../lib/graphIpc", () => ({ graphCommands: graph, graphEvents: { graph: vi.fn(), graphResync: vi.fn() } }));

const { editors } = await import("../../app/editors");
const { registry } = await import("../../app/registry");
const { ResearchInspector } = await import("./Inspector");
const { useSpace } = await import("./space");

const listing = (workflows: ResearchListing["workflows"]): ResearchListing => ({
  space: null,
  sources: [],
  data: [],
  notes: [],
  references: [],
  experiments: [],
  results: [],
  workflows,
  truncated: false,
  graphRevision: 3,
  graphUnavailable: null,
});

registry.registerEditor({
  id: "test.workflows.editor",
  priority: 100,
  matches: (resource) => resource.key.startsWith("workflow:"),
  modes: ["source"],
  component: () => null,
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("WorkflowInspector", () => {
  it("opens the workflow editor with the workflow id, not the entity id", () => {
    const open = vi.spyOn(editors, "open").mockImplementation(() => undefined as never);
    useSpace.setState({ listing: listing([{ entity: "ent-0a1b", workflow: "wf-9f8e", label: "Check OddSums.lean", lifecycle: "active" }]) });
    render(<ResearchInspector item={{ kind: "workflow", entity: "ent-0a1b" }} />);
    expect(screen.getByTestId("research-inspector").getAttribute("data-entity")).toBe("ent-0a1b");
    fireEvent.click(screen.getByRole("button", { name: "Open in the workflow editor" }));
    expect(open).toHaveBeenCalledTimes(1);
    expect(open).toHaveBeenCalledWith({ key: "workflow:wf-9f8e", title: "Check OddSums.lean" });
  });

  it("offers nothing to open for an entity that names no saved workflow, and says why", () => {
    const open = vi.spyOn(editors, "open").mockImplementation(() => undefined as never);
    useSpace.setState({ listing: listing([{ entity: "ent-0a1b", workflow: null, label: "Orphan", lifecycle: "active" }]) });
    render(<ResearchInspector item={{ kind: "workflow", entity: "ent-0a1b" }} />);
    expect(screen.queryByRole("button", { name: "Open in the workflow editor" })).toBeNull();
    expect(screen.getByText("No saved definition for this workflow")).toBeTruthy();
    expect(open).not.toHaveBeenCalled();
  });

  it("says when the selected workflow is no longer listed", () => {
    useSpace.setState({ listing: listing([]) });
    render(<ResearchInspector item={{ kind: "workflow", entity: "ent-gone" }} />);
    expect(screen.getByText("Workflow not found")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Open in the workflow editor" })).toBeNull();
  });
});
