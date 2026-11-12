// The Agents page and the Agent view are two views of one service and store:
// one task list, one approval (answerable from either), the graph context the
// agent was given (consent filtering visible), its graph proposals, and
// keeping a task as an investigation.
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GraphContext } from "../../bindings/GraphContext";
import type { TaskDetail } from "../../bindings/TaskDetail";
import type { TaskGraphInfo } from "../../bindings/TaskGraphInfo";
import type { TaskProposal } from "../../bindings/TaskProposal";
import type { TaskSummary } from "../../bindings/TaskSummary";

const ipc = vi.hoisted(() => ({
  agentsTaskDetail: vi.fn(),
  agentsProviderStatus: vi.fn(),
  agentsTaskList: vi.fn(),
  agentsApprovalResolve: vi.fn(),
  agentsTaskCancel: vi.fn(),
  workspaceCurrent: vi.fn(),
}));
const graphIpc = vi.hoisted(() => ({
  keepInvestigation: vi.fn(),
  createGenerationTask: vi.fn(),
  keptEntities: vi.fn(),
}));

vi.mock("../../lib/ipc", () => ({
  commands: ipc,
  events: { agents: vi.fn(), agentsResync: vi.fn(), workspace: vi.fn(), documents: vi.fn() },
  errorMessage: (e: unknown) => String(e),
}));
vi.mock("./graphIpc", () => ({ agentGraph: graphIpc }));
vi.mock("./DiffView", () => ({ DiffView: () => null }));
// The first load never completes, so it cannot overwrite the state set here.
ipc.agentsProviderStatus.mockReturnValue(new Promise(() => {}));

const { DEFAULT_BUDGETS } = await import("./TaskComposer");
const { initialAgentsState, openApproval, useAgents, AGENTS_PAGE_ID } = await import("./store");
const { AgentsPage, keptTaskIds, taskMatches } = await import("./AgentsPage");
const { AgentPanel } = await import("./AgentPanel");
const { GraphContextView, ProposalList, freshnessLabel, proposalLabel } = await import("./GraphViews");
const { startProjectGeneration } = await import("./generation");
const { EDITOR_PAGE, pages, resetPagesForTests, usePages } = await import("../../app/pages");
// The feature's registrations (the Agents page among them), as the app loads them.
await import("./register");

function summary(overrides: Partial<TaskSummary> = {}): TaskSummary {
  return {
    id: "task-1",
    title: "Relate the lemma",
    createdAtMs: 1,
    updatedAtMs: 1,
    status: { kind: "awaitingApproval" },
    spec: {
      prompt: "Relate the lemma",
      workspaceRoot: "C:/paper",
      scope: { kind: "workspace" },
      permission: "reviewEdits",
      execution: "disabled",
      consent: "metadataOnly",
      budgets: DEFAULT_BUDGETS,
      provider: { provider: "scripted", model: "claude-opus-5-5", effort: "medium" },
    },
    usage: {
      turns: 1,
      inputTokens: 1,
      outputTokens: 1,
      cacheReadTokens: 0,
      cacheWriteTokens: 0,
      toolCalls: 1,
      commands: 0,
      elapsedMs: 0,
      servedBy: null,
    },
    pendingReview: 0,
    changedFiles: 0,
    ...overrides,
  };
}

const context: GraphContext = {
  project: "prj-0123456789abcdef0123456789abcdef",
  revision: 7,
  temporary: false,
  freshness: "pending",
  pending: 0,
  scope: "the whole project",
  consent: "metadataOnly",
  coverage: { resources: 1, complete: 0, partial: 0, unsupported: 1, pending: 0, failed: 0, unresolvedBindings: 0 },
  counts: [
    { kind: "resource", count: 1 },
    { kind: "lemma", count: 1 },
  ],
  entities: [
    {
      id: "ent-00000000000000000000000000000001",
      kind: "resource",
      roles: ["manuscript"],
      label: "main.typ",
      path: "main.typ",
      lifecycle: "active",
      revision: 1,
      origin: "service",
      version: { contentHash: "ab", revision: null, saved: true },
      binding: "resolved",
      coverage: null,
      evidence: [],
    },
    {
      id: "ent-00000000000000000000000000000002",
      kind: "lemma",
      roles: [],
      label: null,
      path: "main.typ",
      lifecycle: "active",
      revision: 1,
      origin: "user",
      version: null,
      binding: "unresolved",
      coverage: null,
      evidence: [{ kind: "formalCheck", freshness: "unknown" }],
    },
  ],
  relations: [
    {
      id: "rel-00000000000000000000000000000003",
      kind: "references",
      from: "ent-00000000000000000000000000000002",
      to: "ent-00000000000000000000000000000001",
      status: "suggested",
      origin: "agent",
      note: null,
    },
  ],
  truncated: false,
  operations: [
    { tool: "graph_query", allowed: true, detail: "bounded reads within the scope; paths, kinds, and counts only" },
    { tool: "read_file", allowed: false, detail: "refused: metadata-only consent" },
  ],
  createdAtMs: 1,
};

const proposal: TaskProposal = {
  toolUseId: "t1",
  proposal: "prp-00000000000000000000000000000004",
  state: "awaitingApproval",
  summary: null,
  baseRevision: 7,
  effects: ["Suggest: “Claim” depends on “Lemma”"],
  operations: [{ op: "addRelation", kind: "dependsOn" }],
  diagnostics: [],
  revision: null,
  created: [],
  message: null,
  createdAtMs: 1,
  updatedAtMs: 1,
};

const graph: TaskGraphInfo = {
  project: context.project,
  context,
  contexts: 1,
  targets: [
    { path: "main.typ", entity: "ent-00000000000000000000000000000001", uses: ["read"], note: null, operation: null, updatedAtMs: 1 },
    { path: "main.log", entity: null, uses: ["read"], note: "not a project resource; an operational reference only", operation: null, updatedAtMs: 1 },
  ],
  proposals: [proposal],
  investigation: null,
};

const approval = {
  id: "approval-1",
  taskId: "task-1",
  toolUseId: "t1",
  tool: "graph_propose",
  title: "Propose 1 graph change",
  detail: "Effects:\n- Suggest: “Claim” depends on “Lemma”\n\nOperations (complete):\n[{\"op\": \"addRelation\"}]",
};

function detail(overrides: Partial<TaskDetail> = {}): TaskDetail {
  return { summary: summary(), transcript: [], approvals: [approval], graph, ...overrides };
}

beforeEach(() => {
  resetPagesForTests();
  Element.prototype.scrollIntoView ??= function scrollIntoView() {};
  const d = detail();
  ipc.agentsTaskDetail.mockResolvedValue(d);
  ipc.agentsApprovalResolve.mockResolvedValue(undefined);
  graphIpc.keptEntities.mockResolvedValue({
    project: context.project,
    revision: 7,
    entities: [],
    relations: [],
    truncated: false,
    total: 0,
    coverage: context.coverage,
    freshness: { state: "pending", pending: 0, checkpointMs: null, overlay: [] },
  });
  useAgents.setState({ ...initialAgentsState, tasks: [d.summary], details: { "task-1": d } });
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("Agents page filters and kept work", () => {
  it("filters tasks and recognises kept investigations by task id", () => {
    const kept = keptTaskIds({
      project: null,
      revision: 1,
      entities: [
        {
          id: "ent-x",
          kind: "investigation",
          roles: [],
          label: "Kept",
          key: null,
          lifecycle: "active",
          tombstoneReason: null,
          revision: 1,
          origin: { kind: "user" },
          attributes: { agentTask: "task-1" },
          createdAtMs: 1,
          updatedAtMs: 1,
        },
      ],
      relations: [],
      truncated: false,
      total: 1,
      coverage: context.coverage,
      freshness: { state: "pending", pending: 0, checkpointMs: null, overlay: [] },
    });
    expect([...kept]).toEqual(["task-1"]);
    const waiting = summary();
    const done = summary({ id: "task-2", status: { kind: "completed" }, pendingReview: 0 });
    expect(taskMatches(waiting, "attention", kept)).toBe(true);
    expect(taskMatches(done, "attention", kept)).toBe(false);
    expect(taskMatches(waiting, "active", kept)).toBe(true);
    expect(taskMatches(done, "kept", kept)).toBe(false);
    expect(taskMatches(waiting, "kept", kept)).toBe(true);
  });
});

describe("graph context and proposals", () => {
  it("shows what the agent was given, with withheld labels marked, relations, operations, and targets", () => {
    render(<GraphContextView graph={graph} />);
    const view = screen.getByTestId("agents-graph-context");
    expect(view.dataset.revision).toBe("7");
    expect(view.dataset.consent).toBe("metadataOnly");
    const rows = screen.getAllByTestId("agents-graph-entity");
    expect(rows).toHaveLength(2);
    expect(within(rows[0]!).getByText("main.typ", { selector: "td" })).toBeTruthy();
    expect(within(rows[1]!).getByTestId("agents-graph-withheld").textContent).toContain("withheld");
    expect(screen.getByText(/Metadata only/)).toBeTruthy();
    expect(screen.getByTestId("agents-graph-relation").dataset.status).toBe("suggested");
    const ops = screen.getAllByTestId("agents-graph-operation");
    expect(ops.map((o) => o.dataset.allowed)).toEqual(["true", "false"]);
    const targets = screen.getAllByTestId("agents-target");
    expect(targets[1]!.textContent).toContain("operational reference");
    expect(freshnessLabel("pending").status).toBe("stale");
  });

  it("lists proposals with their state and complete operations", () => {
    render(<ProposalList proposals={[proposal, { ...proposal, toolUseId: "t2", state: "applied", revision: 8 }]} />);
    const items = screen.getAllByTestId("agents-proposal");
    expect(items.map((i) => i.dataset.state)).toEqual(["applied", "awaitingApproval"]);
    expect(items[0]!.textContent).toContain("applied at revision 8");
    expect(items[1]!.textContent).toContain("addRelation");
    expect(proposalLabel("applied").label).toBe("Applied as suggestion");
  });
});

describe("Editor ↔ Agents navigation keeps one task and its approval", () => {
  it("shows the same task and approval on the page and in the panel; answering in one answers it once", async () => {
    render(
      <>
        <AgentPanel />
        <AgentsPage />
      </>,
    );
    act(() => {
      useAgents.setState({ view: "task", selected: "task-1" });
    });
    // On the Agents page.
    await act(async () => pages.show(AGENTS_PAGE_ID));
    const page = screen.getByTestId("agents-page");
    expect(within(page).getAllByTestId("agents-page-task")).toHaveLength(1);
    fireEvent.click(within(page).getByTestId("agents-page-task"));
    expect(useAgents.getState().page.selected).toBe("task-1");
    const panel = screen.getByTestId("agents-panel");
    expect(within(page).getByTestId("agents-approval")).toBeTruthy();
    expect(within(panel).getByTestId("agents-approval")).toBeTruthy();
    // Back to the Editor and again to Agents: still one task, still waiting.
    await act(async () => pages.show(EDITOR_PAGE));
    await act(async () => pages.show(AGENTS_PAGE_ID));
    expect(useAgents.getState().tasks).toHaveLength(1);
    expect(within(page).getByTestId("agents-approval")).toBeTruthy();
    // Approve on the page: the one pending approval is answered once.
    await act(async () => {
      fireEvent.click(within(page).getByTestId("agents-approve"));
    });
    expect(ipc.agentsApprovalResolve).toHaveBeenCalledTimes(1);
    expect(ipc.agentsApprovalResolve).toHaveBeenCalledWith("task-1", "approval-1", true);
  });

  it("opens a pending approval where the user is: on the page when it is showing", async () => {
    render(<AgentsPage />);
    await act(async () => pages.show(AGENTS_PAGE_ID));
    act(() => openApproval("task-1"));
    expect(usePages.getState().active).toBe(AGENTS_PAGE_ID);
    expect(useAgents.getState().page).toMatchObject({ selected: "task-1", tab: "run" });
    expect(useAgents.getState().view).toBe("tasks");
  });

  it("the panel's task view leads to the same task on the Agents page", async () => {
    render(<AgentPanel />);
    act(() => useAgents.setState({ view: "task", selected: "task-1" }));
    fireEvent.click(await screen.findByTestId("agents-open-page"));
    expect(usePages.getState().active).toBe(AGENTS_PAGE_ID);
    expect(useAgents.getState().page).toMatchObject({ selected: "task-1", tab: "graph" });
  });

  it("keeps a stopped task as an investigation", async () => {
    const done = detail({ summary: summary({ status: { kind: "completed" } }), approvals: [] });
    ipc.agentsTaskDetail.mockResolvedValue(done);
    useAgents.setState({ tasks: [done.summary], details: { "task-1": done } });
    graphIpc.keepInvestigation.mockResolvedValue("ent-00000000000000000000000000000009");
    render(<AgentsPage />);
    await act(async () => pages.show(AGENTS_PAGE_ID));
    act(() => useAgents.setState((s) => ({ page: { ...s.page, selected: "task-1" } })));
    await act(async () => {
      fireEvent.click(screen.getByTestId("agents-keep-investigation"));
    });
    expect(graphIpc.keepInvestigation).toHaveBeenCalledWith("task-1");
    expect(graphIpc.keptEntities).toHaveBeenCalled();
  });
});

describe("project generation helper", () => {
  it("starts an ordinary task with execution disabled by default and shows it on the Agents page", async () => {
    graphIpc.createGenerationTask.mockResolvedValue(summary({ id: "task-9", status: { kind: "queued" } }));
    ipc.agentsTaskList.mockResolvedValue([]);
    ipc.workspaceCurrent.mockResolvedValue(null);
    const task = await startProjectGeneration({ objective: "Study prime gaps", workspaceRoot: "C:/new", show: "page" });
    expect(task.id).toBe("task-9");
    expect(graphIpc.createGenerationTask).toHaveBeenCalledWith({
      objective: "Study prime gaps",
      workspaceRoot: "C:/new",
      permission: "reviewEdits",
      execution: null,
      consent: null,
      budgets: null,
    });
    expect(usePages.getState().active).toBe(AGENTS_PAGE_ID);
    expect(useAgents.getState().page.selected).toBe("task-9");
  });
});
