// Design C layout: the one composer (Options summary and sheet, a started
// task's fixed settings, Send waiting while the task works, drafts shared by
// both views), the turn outline (grouping and summaries from recorded tool
// calls and review records only), the "Needs you" jump, and the Agents
// page's right panel.
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { FileReview } from "../../bindings/FileReview";
import type { ProviderStatus } from "../../bindings/ProviderStatus";
import type { QuestionRequest } from "../../bindings/QuestionRequest";
import type { TaskDetail } from "../../bindings/TaskDetail";
import type { TaskGraphInfo } from "../../bindings/TaskGraphInfo";
import type { TaskSummary } from "../../bindings/TaskSummary";
import type { TranscriptItem } from "../../bindings/TranscriptItem";

const ipc = vi.hoisted(() => ({
  agentsTaskDetail: vi.fn(),
  agentsTaskCreate: vi.fn(),
  agentsTaskContinue: vi.fn(),
  agentsTaskCancel: vi.fn(),
  agentsReview: vi.fn(),
  agentsReviewDiff: vi.fn(),
  agentsProviderStatus: vi.fn(),
  agentsTaskList: vi.fn(),
  workspaceCurrent: vi.fn(),
  agentsAttachmentPreview: vi.fn(),
  agentsQuestionAnswer: vi.fn(),
  agentsQuestionDiscuss: vi.fn(),
}));
const graphIpc = vi.hoisted(() => ({ keepInvestigation: vi.fn(), keptEntities: vi.fn(), createGenerationTask: vi.fn() }));
vi.mock("../../lib/ipc", () => ({
  commands: ipc,
  events: { agents: vi.fn(), agentsResync: vi.fn(), workspace: vi.fn(), documents: vi.fn() },
  errorMessage: (e: unknown) => (e instanceof Error ? e.message : String(e)),
}));
vi.mock("./graphIpc", () => ({ agentGraph: graphIpc }));
vi.mock("./DiffView", () => ({ DiffView: () => null }));
vi.mock("./ModelMenu", () => ({ ModelMenu: () => <div data-testid="model-menu" /> }));
// The real figure card loads Research (and the editor); its own tests cover it.
vi.mock("./FigureCard", () => ({
  FigureCard: ({ figure }: { figure: { name: string } }) => <figure data-testid="agents-figure">{figure.name}</figure>,
}));
// The first load never completes, so it cannot overwrite the state set here.
ipc.agentsProviderStatus.mockReturnValue(new Promise(() => {}));

const { ANSWER_FIRST, DEFAULT_BUDGETS, optionsSummary, resetComposerForTests, SEND_WAITS, TaskComposer } = await import("./TaskComposer");
const { QUESTION_BUSY_TEXT, resetQuestionMemoryForTests } = await import("./questions");
const { TaskView } = await import("./TaskView");
const { AgentsPage, changesLabel, proposalsLabel } = await import("./AgentsPage");
const { groupTurns, turnSummary } = await import("./turns");
const { resetOutlineForTests } = await import("./TurnOutline");
const { drafts, NEW_TASK_DRAFT } = await import("./drafts");
const { initialAgentsState, useAgents, AGENTS_PAGE_ID } = await import("./store");
const { pages, resetPagesForTests } = await import("../../app/pages");

const NO_GRAPH: TaskGraphInfo = { project: null, context: null, contexts: 0, targets: [], proposals: [], investigation: null };

const status: ProviderStatus = {
  settings: { provider: "anthropic", model: "claude-opus-5-5", effort: "high", consent: "scopeContent" },
  configured: true,
  connections: [{ provider: "anthropic", auth: "apiKey", configured: true, credentialError: null, context: null, version: 1 }],
  credentialStore: "test store",
  credentialError: null,
  scriptedAvailable: false,
};

function summary(overrides: Partial<TaskSummary> = {}): TaskSummary {
  return {
    id: "task-1",
    title: "Fit the rates",
    createdAtMs: 1,
    updatedAtMs: 1,
    status: { kind: "completed" },
    spec: {
      prompt: "Fit the rates",
      workspaceRoot: "C:/paper",
      scope: { kind: "document", path: "main.tex" },
      permission: "autoApply",
      execution: "approveEach",
      consent: "metadataOnly",
      budgets: DEFAULT_BUDGETS,
      provider: { provider: "anthropic", model: "claude-opus-5-5", effort: "high" },
      approvalMode: "ask",
    },
    usage: { turns: 38, inputTokens: 10, outputTokens: 10, cacheReadTokens: 0, cacheWriteTokens: 0, toolCalls: 3, commands: 1, elapsedMs: 1000, servedBy: null },
    pendingReview: 0,
    changedFiles: 0,
    ...overrides,
  };
}

type Call = Extract<TranscriptItem, { kind: "toolCall" }>;
const call = (seq: number, name: string, input: unknown, status: Call["status"] = "succeeded", extra: Partial<Call> = {}): Call => ({
  kind: "toolCall",
  seq,
  toolUseId: `t${seq}`,
  name,
  input,
  status,
  result: status === "failed" ? "error: no such file" : "ok",
  isError: status === "failed",
  authorizedBy: null,
  ...extra,
});

function question(id: string, toolUseId: string, prompt = "Which unit?"): QuestionRequest {
  return {
    id,
    taskId: "task-1",
    toolUseId,
    revision: 1,
    questions: [
      {
        id: "q1",
        prompt,
        explanation: null,
        options: [
          { id: "kelvin", label: "Kelvin", consequence: null, recommended: true },
          { id: "celsius", label: "Celsius", consequence: null, recommended: false },
        ],
        allowCustom: true,
      },
    ],
    state: { kind: "pending" },
    createdAtMs: 1,
  };
}

function load(detail: Partial<TaskDetail> & { summary: TaskSummary }) {
  const full: TaskDetail = { transcript: [], approvals: [], graph: NO_GRAPH, allowances: [], ...detail };
  ipc.agentsTaskDetail.mockResolvedValue(full);
  useAgents.setState({ details: { [full.summary.id]: full }, tasks: [full.summary] });
  return full;
}

const review = (path: string, overrides: Partial<FileReview> & { unsaved?: boolean | null } = {}): FileReview =>
  ({ path, kind: "modified", state: "pending", changeCount: 1, entangled: false, tipRevision: 1, currentRevision: 1, message: null, ...overrides }) as FileReview;

beforeEach(() => {
  for (const fn of Object.values(ipc)) fn.mockReset();
  ipc.agentsProviderStatus.mockReturnValue(new Promise(() => {}));
  ipc.agentsReview.mockResolvedValue({ taskId: "task-1", checkpoint: null, files: [] });
  ipc.agentsTaskCancel.mockResolvedValue(undefined);
  graphIpc.keptEntities.mockResolvedValue({ project: null, revision: 1, entities: [], relations: [], truncated: false, total: 0, coverage: null, freshness: null });
  Element.prototype.scrollIntoView ??= function scrollIntoView() {};
  drafts.resetForTests();
  resetComposerForTests();
  resetOutlineForTests();
  resetQuestionMemoryForTests();
  resetPagesForTests();
  useAgents.setState({ ...initialAgentsState, status, workspace: { root: "C:/paper", name: "paper" } as never });
});
afterEach(cleanup);

const options = () => screen.getByTestId("agents-options");
function openOptions(tab: "Model" | "Permissions" | "Budgets") {
  if (options().getAttribute("aria-expanded") !== "true") fireEvent.click(options());
  fireEvent.click(screen.getByRole("tab", { name: new RegExp(`^${tab}`) }));
}

describe("composer Options", () => {
  it("summarizes model · effort · approvals · document changes, custom budgets, and native execution", async () => {
    expect(
      optionsSummary({ model: { provider: "anthropic", model: "m", effort: "xhigh" }, approvals: "Ask", changes: "Read only", customBudgets: true }),
    ).toBe("m · Extra high · Ask · Read only · custom budgets");
    expect(optionsSummary({ model: { provider: "scripted", model: "x", effort: "low" }, approvals: "Auto", changes: null, customBudgets: false })).toBe(
      "Scripted provider · Auto",
    );

    render(<TaskComposer onCreated={() => {}} />);
    expect(options().textContent).toBe("Options: claude-opus-5-5 · High · Ask · Ask before each change");
    expect(options().getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByTestId("agents-native-badge")).toBeNull();

    openOptions("Budgets");
    expect(options().getAttribute("aria-expanded")).toBe("true");
    expect(screen.getAllByRole("tab").map((t) => t.textContent)).toEqual(["Model", "Permissions", "Budgets"]);
    fireEvent.change(screen.getByRole("spinbutton", { name: /Model turns/ }), { target: { value: "60" } });
    expect(options().textContent).toContain("· custom budgets");
    expect(screen.getByText("4,000,000 (fixed)")).toBeTruthy();

    openOptions("Permissions");
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: /Commands and code/ }));
    });
    await act(async () => {
      fireEvent.click(await screen.findByRole("option", { name: "Ask before each command (runs natively)" }));
    });
    expect(screen.getByTestId("agents-native-badge").textContent).toBe("Native");
    // The sheet separates this task, (Auto) the project's permissions, and the defaults.
    const panel = screen.getByTestId("agents-permissions-panel");
    expect(within(panel).getAllByRole("heading").map((h) => h.textContent)).toEqual(["This task", "Defaults for future tasks"]);
    fireEvent.click(within(panel).getByRole("radio", { name: "Auto" }));
    expect(within(panel).getAllByRole("heading").map((h) => h.textContent)).toEqual([
      "This task",
      "This project's Auto permissions",
      "Defaults for future tasks",
    ]);
  });

  it("starts a new task with the sheet's settings, then returns to the defaults", async () => {
    ipc.agentsTaskCreate.mockResolvedValue(summary({ id: "task-9" }));
    render(<TaskComposer onCreated={() => {}} />);
    fireEvent.change(screen.getByTestId("agents-prompt"), { target: { value: "Fit" } });
    openOptions("Budgets");
    fireEvent.change(screen.getByRole("spinbutton", { name: /Commands/ }), { target: { value: "5" } });
    // Ctrl+Enter sends; Enter alone is a new line.
    fireEvent.keyDown(screen.getByTestId("agents-prompt"), { key: "Enter" });
    expect(ipc.agentsTaskCreate).not.toHaveBeenCalled();
    await act(async () => {
      fireEvent.keyDown(screen.getByTestId("agents-prompt"), { key: "Enter", ctrlKey: true });
    });
    expect(ipc.agentsTaskCreate).toHaveBeenCalledWith(
      expect.objectContaining({ prompt: "Fit", budgets: { ...DEFAULT_BUDGETS, maxCommands: 5 }, scope: { kind: "workspace" }, consent: "scopeContent" }),
    );
    expect(options().textContent).not.toContain("custom budgets");
  });

  it("shows a started task's settings as fixed, its allowances, and its usage", () => {
    load({
      summary: summary({ status: { kind: "budgetExceeded", budget: "turns", detail: "the limit of 40 model turns was reached." } }),
      allowances: [{ kind: "nativeCode", grantedAtMs: 1, approved: "Run Python code" }],
    });
    render(<TaskView id="task-1" />);
    expect(options().textContent).toBe("Options: claude-opus-5-5 · High · Ask · Apply changes, review afterwards");
    expect(screen.getByTestId("agents-native-badge")).toBeTruthy();
    openOptions("Permissions");
    const settings = screen.getByTestId("agents-task-settings");
    expect(settings.textContent).toContain("One document: main.tex");
    expect(settings.textContent).toContain("File names only");
    expect(settings.textContent).toContain("Fixed when the task started.");
    expect(screen.queryByTestId("agents-permission")).toBeNull();
    expect(screen.queryByRole("radio", { name: "Auto" })).toBeNull();
    expect(screen.getByTestId("agents-allowances").textContent).toContain("Native code");
    openOptions("Budgets");
    expect(screen.getByRole("progressbar", { name: "Turns" })).toBeTruthy();
    expect(screen.queryByRole("spinbutton", { name: /Model turns/ })).toBeNull();
    const meter = screen.getByTestId("agents-budget-meter");
    expect(meter.textContent).toContain("Stopped: turn budget");
    expect(meter.textContent).toContain("A follow-up keeps the same budgets and the usage so far.");
  });

  it("keeps typing while the task works, but Send waits; Stop cancels", async () => {
    load({ summary: summary({ status: { kind: "running" } }) });
    render(<TaskView id="task-1" />);
    const field = screen.getByTestId("agents-follow-up-text");
    fireEvent.change(field, { target: { value: "Also add error bars" } });
    expect((field as HTMLTextAreaElement).value).toBe("Also add error bars");
    const send = screen.getByTestId("agents-continue") as HTMLButtonElement;
    expect(send.disabled).toBe(true);
    expect(send.getAttribute("data-variant")).toBe("secondary");
    expect(screen.getByTestId("agents-send-waits").textContent).toBe(SEND_WAITS);
    await act(async () => {
      fireEvent.keyDown(field, { key: "Enter", ctrlKey: true });
    });
    expect(ipc.agentsTaskContinue).not.toHaveBeenCalled();
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Stop" }));
    });
    expect(ipc.agentsTaskCancel).toHaveBeenCalledWith("task-1");
  });

  it("shares drafts and new-task settings between the Agent view and the Agents page", async () => {
    load({ summary: summary() });
    const panel = render(<TaskView id="task-1" />);
    fireEvent.change(screen.getByTestId("agents-follow-up-text"), { target: { value: "Follow up here" } });
    panel.unmount();
    render(<TaskView id="task-1" variant="page" />);
    expect((screen.getByTestId("agents-follow-up-text") as HTMLTextAreaElement).value).toBe("Follow up here");
    cleanup();

    const first = render(<TaskComposer />);
    fireEvent.change(screen.getByTestId("agents-prompt"), { target: { value: "New idea" } });
    openOptions("Budgets");
    fireEvent.change(screen.getByRole("spinbutton", { name: /Minutes/ }), { target: { value: "45" } });
    first.unmount();
    render(<TaskComposer onCreated={() => {}} />);
    expect((screen.getByTestId("agents-prompt") as HTMLTextAreaElement).value).toBe("New idea");
    expect(options().textContent).toContain("custom budgets");
    expect(drafts.get(NEW_TASK_DRAFT).text).toBe("New idea");
  });
});

describe("turn outline", () => {
  const transcript: TranscriptItem[] = [
    { kind: "userText", seq: 1, text: "Fit the rates", attachments: [] },
    { kind: "assistantText", seq: 2, text: "I will read the data and say I edited everything." },
    call(3, "read_file", { path: "data/rates.csv" }),
    call(4, "read_file", { path: "main.tex" }),
    call(5, "read_file", { path: "main.tex" }),
    call(6, "edit_document", { path: "sections/main.tex", edits: [] }),
    call(7, "run_python", { code: "fit()" }, "succeeded", { figures: [{ runId: "r", artifactId: "a", mediaType: "image/png", name: "fig.png", caption: null }] } as never),
    call(8, "create_file", { path: "plot.py" }, "declined"),
    { kind: "assistantText", seq: 9, text: "Done." },
    { kind: "userText", seq: 10, text: "Now the appendix", attachments: [] },
    call(11, "read_file", { path: "appendix.tex" }, "failed"),
    { kind: "notice", seq: 12, level: "error", message: "The provider failed." },
  ];

  it("groups messages into turns and summarizes recorded outcomes, never the prose", () => {
    const groups = groupTurns(transcript);
    expect(groups.map((g) => g.kind)).toEqual(["user", "agent", "user", "agent"]);
    const calls = (groups[1]!.kind === "agent" ? groups[1]!.entries : []).flatMap((e) => (e.item.kind === "toolCall" ? [e.item] : []));
    const reviews = new Map([["sections/main.tex", review("sections/main.tex", { unsaved: true })]]);
    const s = turnSummary(calls, reviews);
    // Unique files read; the declined create is not counted as done.
    expect(s.text).toBe("Read 2 files · Edited main.tex — unsaved · Ran Python · 1 figure");
    expect(s.declined).toBe(1);
    expect(s.failed).toBe(0);
    expect(turnSummary(calls).text).toBe("Read 2 files · Edited main.tex · Ran Python · 1 figure");
    expect(turnSummary(calls, new Map([["sections/main.tex", review("sections/main.tex", { unsaved: false })]])).text).toContain(
      "Edited main.tex — saved",
    );
    expect(turnSummary([call(1, "edit_document", { path: "a" }, "awaitingApproval")]).text).toBe("1 action");
    expect(turnSummary([call(1, "edit_document", { path: "a" }, "awaitingApproval")]).waiting).toBe(1);
  });

  it("keeps text visible, collapses routine rows under the header, and opens failed turns", async () => {
    load({ summary: summary(), transcript });
    render(<TaskView id="task-1" />);
    const turns = screen.getAllByTestId("agents-turn");
    expect(turns).toHaveLength(2);
    // First turn: collapsed; its text stays, its tool rows are hidden.
    const [first, second] = turns as [HTMLElement, HTMLElement];
    const toggle = within(first).getByTestId("agents-turn-toggle");
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(within(first).getByText(/say I edited everything/)).toBeTruthy();
    expect(within(first).getByText("Done.")).toBeTruthy();
    expect(within(first).queryAllByTestId("agents-tool-call")).toHaveLength(0);
    expect(within(first).getByText("1 declined")).toBeTruthy();
    fireEvent.click(toggle);
    expect(within(first).getAllByTestId("agents-tool-call")).toHaveLength(6);
    // The failed turn starts expanded, with its error.
    expect(within(second).getByTestId("agents-turn-toggle").getAttribute("aria-expanded")).toBe("true");
    expect(within(second).getByText("1 failed")).toBeTruthy();
    expect(within(second).getByText("The provider failed.")).toBeTruthy();
    // Reopening (the page) shows the same expansion and no duplicate entries.
    cleanup();
    render(<TaskView id="task-1" variant="page" />);
    expect(screen.getAllByTestId("agents-turn")).toHaveLength(2);
    expect(screen.getAllByTestId("agents-tool-call")).toHaveLength(7);
    expect(screen.getAllByText("Done.")).toHaveLength(1);
  });

  it("shows an ask_user call as a tool row until its question request is recorded", () => {
    load({
      summary: summary(),
      transcript: [{ kind: "userText", seq: 1, text: "Go", attachments: [] }, call(2, "read_file", { path: "a.tex" }), call(3, "ask_user", { questions: [] }, "running")],
    });
    render(<TaskView id="task-1" />);
    const anchor = document.querySelector("[data-needs-you='question:t3']");
    expect(anchor).toBeTruthy();
    expect(within(anchor as HTMLElement).getByTestId("agents-tool-call").dataset.tool).toBe("ask_user");
    expect(screen.getByTestId("agents-turn-summary").textContent).toBe("Read a.tex");
  });

  it("renders the question card at its call, always visible while the turn is collapsed", () => {
    load({
      summary: summary({ status: { kind: "awaitingAnswer" } }),
      transcript: [{ kind: "userText", seq: 1, text: "Go", attachments: [] }, call(2, "read_file", { path: "a.tex" }), call(3, "ask_user", { questions: [] }, "running")],
      questions: [question("req-1", "t3")],
    });
    render(<TaskView id="task-1" />);
    expect(screen.getByTestId("agents-turn-toggle").getAttribute("aria-expanded")).toBe("false");
    const anchor = document.querySelector("[data-needs-you='question:t3']") as HTMLElement;
    const card = within(anchor).getByTestId("agents-question");
    expect(card.dataset.question).toBe("req-1");
    expect(within(card).getByText("Needs your answer")).toBeTruthy();
    expect(within(anchor).queryByTestId("agents-tool-call")).toBeNull();
    expect(screen.getByTestId("agents-task-status").dataset.status).toBe("awaitingAnswer");
  });

  it("shows a call's figures beside it, outside the collapsed activity", async () => {
    const figure = { runId: "run-1", artifactId: "art-1", mediaType: "image/png", name: "fit.png", caption: null };
    load({
      summary: summary(),
      transcript: [{ kind: "userText", seq: 1, text: "Plot it", attachments: [] }, call(2, "run_python", { code: "plot()" }, "succeeded", { figures: [figure] })],
    });
    render(<TaskView id="task-1" />);
    expect(screen.getByTestId("agents-turn-toggle").getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByTestId("agents-tool-call")).toBeNull();
    expect((await screen.findByTestId("agents-figure")).textContent).toBe("fit.png");
    expect(screen.getByTestId("agents-turn-summary").textContent).toBe("Ran Python · 1 figure");
  });
});

describe("Needs you", () => {
  it("jumps to an inline approval and moves focus to it", () => {
    const approval = { id: "approval-1", taskId: "task-1", toolUseId: "t2", tool: "run_python", title: "Run Python code", detail: "print(1)", reason: null, forTask: [] };
    load({
      summary: summary({ status: { kind: "awaitingApproval" } }),
      transcript: [{ kind: "userText", seq: 1, text: "Go", attachments: [] }, call(2, "run_python", { code: "print(1)" }, "awaitingApproval")],
      approvals: [approval],
    });
    const scroll = vi.spyOn(Element.prototype, "scrollIntoView");
    render(<TaskView id="task-1" />);
    const needs = screen.getByRole("navigation", { name: "Needs you" });
    const link = within(needs).getByRole("link", { name: "Waiting for approval: Run Python code" });
    fireEvent.click(link);
    const card = screen.getByTestId("agents-approval");
    expect(document.activeElement).toBe(card);
    expect(scroll).toHaveBeenCalled();
    // The card is inline at its call, inside the conversation.
    expect(screen.getByTestId("agents-transcript").contains(card)).toBe(true);
    scroll.mockRestore();
  });

  it("offers the review once the task has stopped with changes, with unsaved files counted", async () => {
    ipc.agentsReview.mockResolvedValue({
      taskId: "task-1",
      checkpoint: null,
      files: [review("main.tex", { unsaved: true }), review("plot.py", { unsaved: false })],
    });
    load({ summary: summary({ pendingReview: 2, changedFiles: 2 }) });
    render(<TaskView id="task-1" />);
    const row = await screen.findByRole("link", { name: "Review changes: 2 files · 1 unsaved" });
    fireEvent.click(row);
    expect(useAgents.getState()).toMatchObject({ view: "review", selected: "task-1" });
  });
});

describe("questions in Needs you and the composer", () => {
  const asked = () =>
    load({
      summary: summary({ status: { kind: "awaitingAnswer" } }),
      transcript: [{ kind: "userText", seq: 1, text: "Go", attachments: [] }, call(2, "ask_user", { questions: [] }, "running")],
      questions: [question("req-1", "t2", "Which temperature unit should Table 2 use in the final version of the paper?")],
    });

  it("lists a waiting question with its short prompt and jumps to the card", () => {
    asked();
    const scroll = vi.spyOn(Element.prototype, "scrollIntoView");
    render(<TaskView id="task-1" />);
    const needs = screen.getByRole("navigation", { name: "Needs you" });
    const link = within(needs).getByTestId("agents-needs-question");
    expect(link.getAttribute("aria-label")).toBe("Needs your answer: Which temperature unit should Table 2 use in the final…");
    fireEvent.click(link);
    expect(document.activeElement).toBe(screen.getByTestId("agents-question"));
    expect(scroll).toHaveBeenCalled();
    scroll.mockRestore();
  });

  it("while a question waits, Send takes only a discussion; Chat about it adds the About chip and focuses the field", async () => {
    asked();
    ipc.agentsQuestionDiscuss.mockResolvedValue(undefined);
    render(<TaskView id="task-1" />);
    const field = screen.getByTestId("agents-follow-up-text") as HTMLTextAreaElement;
    fireEvent.change(field, { target: { value: "Unrelated idea" } });
    const send = screen.getByTestId("agents-continue") as HTMLButtonElement;
    expect(send.disabled).toBe(true);
    expect(screen.getByTestId("agents-send-waits").textContent).toBe(ANSWER_FIRST);

    fireEvent.click(screen.getByRole("button", { name: /Chat about it/ }));
    const chip = screen.getByTestId("agents-about");
    expect(chip.textContent).toContain("About: Which temperature unit should Table 2 use in the final version of the paper?");
    expect(chip.dataset.tone).toBe("accent");
    expect(document.activeElement).toBe(field);
    // The existing draft text stays; the hint about answering is gone.
    expect(field.value).toBe("Unrelated idea");
    expect(screen.queryByTestId("agents-send-waits")).toBeNull();
    expect(send.disabled).toBe(false);

    fireEvent.change(field, { target: { value: "Why Kelvin?" } });
    await act(async () => {
      fireEvent.click(send);
    });
    expect(ipc.agentsQuestionDiscuss).toHaveBeenCalledWith("task-1", "req-1", 1, "Why Kelvin?");
    expect(ipc.agentsTaskContinue).not.toHaveBeenCalled();
    // Sent: the About is gone and the unrelated draft comes back.
    expect(screen.queryByTestId("agents-about")).toBeNull();
    expect(field.value).toBe("Unrelated idea");
    expect(send.disabled).toBe(true);
  });

  it("shows a refused discussion in the composer and keeps the message; removing the chip stops discussing", async () => {
    asked();
    ipc.agentsQuestionDiscuss.mockRejectedValue({ code: "agents.questionBusy", message: "busy" });
    render(<TaskView id="task-1" />);
    fireEvent.click(screen.getByRole("button", { name: /Chat about it/ }));
    const field = screen.getByTestId("agents-follow-up-text") as HTMLTextAreaElement;
    fireEvent.change(field, { target: { value: "Why Kelvin?" } });
    await act(async () => {
      fireEvent.click(screen.getByTestId("agents-continue"));
    });
    expect(screen.getByTestId("agents-discussion-error").textContent).toContain(QUESTION_BUSY_TEXT);
    expect(field.value).toBe("Why Kelvin?");
    expect(screen.getByTestId("agents-about")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: /^Remove About:/ }));
    expect(screen.queryByTestId("agents-about")).toBeNull();
    expect(screen.queryByTestId("agents-discussion-error")).toBeNull();
    expect(field.value).toBe("Why Kelvin?");
    expect((screen.getByTestId("agents-continue") as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByTestId("agents-send-waits").textContent).toBe(ANSWER_FIRST);
  });
});

describe("Agents page", () => {
  it("opens Changes, Graph context, and Proposals in one right panel beside the conversation", async () => {
    ipc.agentsReview.mockResolvedValue({ taskId: "task-1", checkpoint: null, files: [review("main.tex", { unsaved: true })] });
    load({ summary: summary({ changedFiles: 3, pendingReview: 1 }) });
    useAgents.setState((s) => ({ page: { ...s.page, selected: "task-1" } }));
    render(<AgentsPage />);
    await act(async () => pages.show(AGENTS_PAGE_ID));
    expect(changesLabel(3, 2)).toBe("Changes 3 · 2 unsaved");
    expect(proposalsLabel(2, 1)).toBe("Proposals 2 · 1 waiting");
    const changes = await screen.findByRole("button", { name: "Changes 3 · 1 unsaved" });
    // The conversation is there with no panel open.
    expect(screen.getByTestId("agents-transcript")).toBeTruthy();
    expect(screen.queryByTestId("agents-page-side")).toBeNull();
    fireEvent.click(changes);
    expect(useAgents.getState().page.tab).toBe("changes");
    expect(screen.getByTestId("agents-page-side").dataset.panel).toBe("changes");
    expect(screen.getByTestId("agents-transcript")).toBeTruthy();
    // The review row of "Needs you" is not repeated while the review is open beside.
    expect(screen.queryByTestId("agents-needs-review")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Graph context" }));
    expect(screen.getByTestId("agents-page-side").dataset.panel).toBe("graph");
    fireEvent.click(screen.getByRole("button", { name: "Proposals 0" }));
    expect(screen.getByTestId("agents-page-side").dataset.panel).toBe("proposals");
    // Escape closes the panel; the toggle closes it too.
    fireEvent.keyDown(screen.getByTestId("agents-page-side"), { key: "Escape" });
    expect(screen.queryByTestId("agents-page-side")).toBeNull();
    expect(useAgents.getState().page.tab).toBe("run");
  });

  it("collapses the task list to a rail and starts a new task from it", async () => {
    render(<AgentsPage />);
    await act(async () => pages.show(AGENTS_PAGE_ID));
    expect(screen.getByTestId("agents-page-compose")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Collapse task list" }));
    expect(screen.getByTestId("agents-page").dataset.list).toBe("rail");
    expect(screen.queryByTestId("agents-page-tasks")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Show task list" }));
    expect(screen.getByTestId("agents-page").dataset.list).toBe("open");
    await waitFor(() => expect(screen.getByRole("heading", { level: 1, name: "Agents" })).toBeTruthy());
  });

  // e2e pages-review: the list region contained the header region, and regions
  // are innermost-only, so F6 never reached the task list.
  it("is three F6 stops: the header, the task list (or its rail), and the detail", async () => {
    const { regionElements } = await import("../../app/shell/regions");
    render(<AgentsPage />);
    await act(async () => pages.show(AGENTS_PAGE_ID));
    const names = () => regionElements().map((r) => r.dataset.region);
    expect(names()).toEqual(expect.arrayContaining(["agents-header", "agents-tasks", "agents-detail"]));
    fireEvent.click(screen.getByRole("button", { name: "Collapse task list" }));
    expect(names()).toEqual(expect.arrayContaining(["agents-tasks", "agents-detail"]));
  });
});
