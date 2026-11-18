// Structured questions: the card (complete answers only, one call at the
// revision, custom answers, stale/resolved refusals, the in-flight guard,
// answered/superseded records, options a revision added), Chat about it and
// the discussion send (the unrelated draft survives), selectors, question
// notifications, and "Waiting for your answer" in statuses and the status bar.
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { QuestionRequest } from "../../bindings/QuestionRequest";
import type { TaskDetail } from "../../bindings/TaskDetail";
import type { TaskSummary } from "../../bindings/TaskSummary";

const ipc = vi.hoisted(() => ({
  agentsQuestionAnswer: vi.fn(),
  agentsQuestionDiscuss: vi.fn(),
  agentsTaskDetail: vi.fn(),
  agentsModelsList: vi.fn(() => new Promise(() => {})),
}));

vi.mock("../../lib/ipc", () => ({
  commands: ipc,
  events: { agents: vi.fn(), agentsResync: vi.fn(), workspace: vi.fn(), documents: vi.fn() },
  errorMessage: (e: unknown) =>
    typeof e === "object" && e !== null && "message" in e ? String((e as { message: unknown }).message) : String(e),
}));

// Monaco needs a real browser (register.ts reaches the review diff).
vi.mock("./DiffView", () => ({ DiffView: () => null }));

const { QuestionCard, continueSentence } = await import("./QuestionCard");
const questions = await import("./questions");
const { drafts, useDrafts } = await import("./drafts");
const store = await import("./store");
const format = await import("./format");
const { DEFAULT_BUDGETS } = await import("./TaskComposer");
const { AgentStatusItem } = await import("./register");
const { useNotifications } = await import("../../app/notifications");
const { layout } = await import("../../app/layout");

const { initialAgentsState, reduceEvent, useAgents, questionNotificationId } = store;

const TASK = "task-1";

function summary(overrides: Partial<TaskSummary> = {}): TaskSummary {
  return {
    id: TASK,
    title: "Fit the Arrhenius model",
    createdAtMs: 1,
    updatedAtMs: 1,
    status: { kind: "awaitingAnswer" },
    spec: {
      prompt: "Fit the Arrhenius model",
      workspaceRoot: "C:/paper",
      scope: { kind: "workspace" },
      permission: "reviewEdits",
      execution: "disabled",
      consent: "scopeContent",
      budgets: DEFAULT_BUDGETS,
      provider: { provider: "anthropic", model: "claude-opus-5-5", effort: "medium" },
      approvalMode: "ask",
    },
    usage: {
      turns: 1,
      inputTokens: 10,
      outputTokens: 10,
      cacheReadTokens: 0,
      cacheWriteTokens: 0,
      toolCalls: 1,
      commands: 0,
      elapsedMs: 1000,
      servedBy: null,
    },
    pendingReview: 0,
    changedFiles: 0,
    ...overrides,
  };
}

let requestSeq = 0;

function request(overrides: Partial<QuestionRequest> = {}): QuestionRequest {
  return {
    id: `req-${++requestSeq}`,
    taskId: TASK,
    toolUseId: "tu-1",
    revision: 1,
    questions: [
      {
        id: "q1",
        prompt: "Which temperature unit should Table 2 use?",
        explanation: "The fit uses Kelvin; the lab notebook records Celsius.",
        options: [
          { id: "kelvin", label: "Kelvin", consequence: "Matches the fit and the figure axis", recommended: true },
          { id: "celsius", label: "Celsius", consequence: "Matches the lab notebook", recommended: false },
        ],
        allowCustom: true,
      },
    ],
    state: { kind: "pending" },
    createdAtMs: 1,
    ...overrides,
  };
}

const twoQuestions = (overrides: Partial<QuestionRequest> = {}) => {
  const base = request(overrides);
  return {
    ...base,
    questions: [
      ...base.questions,
      {
        id: "q2",
        prompt: "Report the activation energy in kJ/mol or eV?",
        explanation: null,
        options: [
          { id: "kjmol", label: "kJ/mol", consequence: "Consistent with Section 2", recommended: true },
          { id: "ev", label: "eV", consequence: null, recommended: false },
        ],
        allowCustom: true,
      },
    ],
  };
};

function detail(requests: QuestionRequest[], status: TaskSummary["status"] = { kind: "awaitingAnswer" }): TaskDetail {
  return {
    summary: summary({ status }),
    transcript: [],
    approvals: [],
    graph: { context: null, targets: [], proposals: [], investigation: null } as unknown as TaskDetail["graph"],
    allowances: [],
    questions: requests,
  };
}

function put(requests: QuestionRequest[]) {
  const d = detail(requests);
  act(() => useAgents.setState({ details: { [TASK]: d }, tasks: [d.summary] }));
  ipc.agentsTaskDetail.mockResolvedValue(d);
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const radio = (id: string) =>
  (screen.getAllByRole("radio") as HTMLInputElement[]).find((r) => r.value === id) as HTMLInputElement;
const sendButton = () => screen.getByRole("button", { name: /Send answer/ });
const disabled = (el: HTMLElement) => el.hasAttribute("disabled") || el.getAttribute("data-disabled") === "true";

beforeEach(() => {
  ipc.agentsQuestionAnswer.mockReset();
  ipc.agentsQuestionDiscuss.mockReset();
  ipc.agentsTaskDetail.mockReset();
});

afterEach(() => {
  cleanup();
  drafts.resetForTests();
  questions.resetQuestionMemoryForTests();
  useNotifications.setState({ items: [] });
  useAgents.setState(initialAgentsState, true);
});

describe("QuestionCard", () => {
  it("shows the question with nothing chosen, Recommended as a badge, and Send answer only once every question has an answer", async () => {
    const r = twoQuestions({ revision: 3 });
    put([r]);
    ipc.agentsQuestionAnswer.mockResolvedValue(undefined);
    render(<QuestionCard task={TASK} request={r} />);

    const card = screen.getByTestId("agents-question");
    expect(card.getAttribute("aria-label")).toBe("Which temperature unit should Table 2 use?");
    expect(card.getAttribute("data-question")).toBe(r.id);
    expect(card.id).toBe(questions.questionAnchorId(r.id));
    expect(card.tabIndex).toBe(-1);
    expect(screen.getByText("Needs your answer")).toBeTruthy();
    expect(screen.getByText("0 of 2 answered")).toBeTruthy();
    expect(screen.getByRole("radiogroup", { name: /Which temperature unit/ })).toBeTruthy();
    expect(screen.getByText("The fit uses Kelvin; the lab notebook records Celsius.")).toBeTruthy();
    expect(screen.getByText("Matches the fit and the figure axis")).toBeTruthy();
    expect(screen.getAllByText("Recommended")).toHaveLength(2);
    expect((screen.getAllByRole("radio") as HTMLInputElement[]).every((radio) => !radio.checked)).toBe(true);
    expect(disabled(sendButton())).toBe(true);

    fireEvent.click(screen.getByText("Celsius"));
    expect(screen.getByText("1 of 2 answered")).toBeTruthy();
    expect(disabled(sendButton())).toBe(true);

    // A custom answer for the second question completes the group.
    fireEvent.change(screen.getByRole("textbox", { name: /Other answer: Report the activation energy/ }), {
      target: { value: "  Both, kJ/mol first  " },
    });
    expect(disabled(sendButton())).toBe(false);
    fireEvent.click(sendButton());
    await waitFor(() => expect(ipc.agentsQuestionAnswer).toHaveBeenCalledTimes(1));
    expect(ipc.agentsQuestionAnswer).toHaveBeenCalledWith(TASK, r.id, 3, [
      { questionId: "q1", value: { kind: "option", optionId: "celsius" } },
      { questionId: "q2", value: { kind: "custom", text: "Both, kJ/mol first" } },
    ]);
    expect(await screen.findByText("Answer sent")).toBeTruthy();
  });

  it("makes one choice per question explicit: typing an answer clears the option, choosing an option clears the text", () => {
    const r = request();
    put([r]);
    render(<QuestionCard task={TASK} request={r} />);
    fireEvent.click(screen.getByText("Kelvin"));
    const other = screen.getByRole("textbox", { name: /Other answer/ }) as HTMLInputElement;
    fireEvent.change(other, { target: { value: "Rankine" } });
    expect(radio("kelvin").checked).toBe(false);
    fireEvent.click(screen.getByText("Celsius"));
    expect(other.value).toBe("");
    expect(radio("celsius").checked).toBe(true);
  });

  it("offers no Other answer when custom answers are not allowed", () => {
    const base = request();
    const r = { ...base, questions: [{ ...base.questions[0]!, allowCustom: false }] };
    put([r]);
    render(<QuestionCard task={TASK} request={r} />);
    expect(screen.queryByRole("textbox")).toBeNull();
  });

  it("sends once on a double click and while another view sends the same answer", async () => {
    const r = request();
    put([r]);
    const pending = deferred<void>();
    ipc.agentsQuestionAnswer.mockReturnValue(pending.promise);
    render(
      <>
        <div data-testid="agents-panel">
          <QuestionCard task={TASK} request={r} />
        </div>
        <div data-testid="agents-page">
          <QuestionCard task={TASK} request={r} />
        </div>
      </>,
    );
    for (const label of screen.getAllByText("Kelvin")) fireEvent.click(label);
    const [first, second] = screen.getAllByRole("button", { name: /Send answer/ });
    fireEvent.click(first!);
    fireEvent.click(first!);
    fireEvent.click(second!);
    await act(async () => {
      pending.resolve();
      await pending.promise;
    });
    expect(ipc.agentsQuestionAnswer).toHaveBeenCalledTimes(1);
    // The guard is shared: a direct second send while one is in flight does nothing.
    const again = deferred<void>();
    ipc.agentsQuestionAnswer.mockReturnValue(again.promise);
    const a = questions.answerQuestion(TASK, r, []);
    const b = await questions.answerQuestion(TASK, r, []);
    expect(b).toBeNull();
    again.resolve();
    await a;
    expect(ipc.agentsQuestionAnswer).toHaveBeenCalledTimes(2);
  });

  it("says what to do after a stale revision and re-reads the task", async () => {
    const r = request();
    put([r]);
    ipc.agentsQuestionAnswer.mockRejectedValue({ code: "agents.questionStale", message: "stale revision" });
    render(<QuestionCard task={TASK} request={r} />);
    fireEvent.click(screen.getByText("Kelvin"));
    fireEvent.click(sendButton());
    const error = await screen.findByTestId("agents-question-error");
    expect(error.textContent).toBe("The question changed; review the updated options.");
    expect(error.getAttribute("data-code")).toBe("agents.questionStale");
    await waitFor(() => expect(ipc.agentsTaskDetail).toHaveBeenCalledWith(TASK));
    // Still answerable.
    expect(disabled(sendButton())).toBe(false);
  });

  it("says an answer from another view already resolved it", async () => {
    const r = request();
    put([r]);
    ipc.agentsQuestionAnswer.mockRejectedValue({ code: "agents.questionResolved", message: "resolved" });
    render(<QuestionCard task={TASK} request={r} />);
    fireEvent.click(screen.getByText("Celsius"));
    fireEvent.click(sendButton());
    expect((await screen.findByTestId("agents-question-error")).textContent).toBe("This question is no longer open.");
    await waitFor(() => expect(ipc.agentsTaskDetail).toHaveBeenCalledWith(TASK));
  });

  it("records an answered question quietly, with no controls", () => {
    const r = twoQuestions({
      state: {
        kind: "answered",
        answers: [
          { questionId: "q1", value: { kind: "option", optionId: "kelvin" } },
          { questionId: "q2", value: { kind: "custom", text: "Both units" } },
        ],
      },
    });
    render(<QuestionCard task={TASK} request={r} />);
    const card = screen.getByTestId("agents-question");
    expect(card.getAttribute("data-open")).toBe("false");
    expect(screen.getAllByTestId("agents-question-answered").map((p) => p.textContent)).toEqual([
      "Answered: Kelvin",
      "Answered: Both units",
    ]);
    expect(screen.queryByRole("button")).toBeNull();
    expect(screen.queryByRole("radio")).toBeNull();
    expect(screen.queryByText("Needs your answer")).toBeNull();
  });

  it("records a superseded question with its reason, with no controls", () => {
    const r = request({ state: { kind: "superseded", reason: "The task was canceled" } });
    render(<QuestionCard task={TASK} request={r} />);
    expect(screen.getByTestId("agents-question-superseded").textContent).toBe("No longer needed — the task was canceled");
    expect(screen.queryByRole("button")).toBeNull();
    expect(continueSentence("Exposition restarted")).toBe("Exposition restarted");
  });

  it("while discussing stays answerable and marks the options a revision added as New", async () => {
    const r1 = request({ revision: 1 });
    put([r1]);
    const { rerender } = render(<QuestionCard task={TASK} request={r1} />);
    expect(screen.queryByText("New")).toBeNull();
    fireEvent.click(screen.getByText("Kelvin"));

    const r2: QuestionRequest = {
      ...r1,
      revision: 2,
      state: { kind: "discussing" },
      questions: [
        {
          ...r1.questions[0]!,
          options: [
            ...r1.questions[0]!.options,
            { id: "kelvin_caption", label: "Kelvin, with °C in the caption", consequence: "Keeps the table narrow", recommended: false },
          ],
        },
      ],
    };
    put([r2]);
    rerender(<QuestionCard task={TASK} request={r2} />);
    expect(screen.getByTestId("agents-question-discussing").textContent).toContain("Discussing");
    const marks = screen.getAllByTestId("agents-question-new");
    expect(marks).toHaveLength(1);
    expect(marks[0]!.closest(".ui-option-list__option")?.textContent).toContain("Kelvin, with °C in the caption");
    // The unchanged option id keeps its choice; sending uses the new revision.
    expect(radio("kelvin").checked).toBe(true);
    ipc.agentsQuestionAnswer.mockResolvedValue(undefined);
    fireEvent.click(sendButton());
    await waitFor(() => expect(ipc.agentsQuestionAnswer).toHaveBeenCalledWith(TASK, r1.id, 2, [
      { questionId: "q1", value: { kind: "option", optionId: "kelvin" } },
    ]));
  });

  it("Chat about it marks the draft and submits nothing", () => {
    const r = request();
    put([r]);
    render(<QuestionCard task={TASK} request={r} />);
    fireEvent.click(screen.getByRole("button", { name: /Chat about it/ }));
    expect(useDrafts.getState().drafts[TASK]?.about).toMatchObject({ requestId: r.id, prompt: r.questions[0]!.prompt });
    expect(ipc.agentsQuestionAnswer).not.toHaveBeenCalled();
    expect(ipc.agentsQuestionDiscuss).not.toHaveBeenCalled();
  });
});

describe("Chat about it drafts", () => {
  const attachment = {
    id: "att-1",
    name: "notes.txt",
    mediaType: "text/plain",
    kind: "text" as const,
    sizeBytes: 10,
    width: null,
    height: null,
    sha256: "0".repeat(64),
    missing: false,
  };

  it("chatAbout keeps the draft text and attachments, and the unrelated text comes back after the discussion", async () => {
    const r = request();
    put([r]);
    useDrafts.setState({
      drafts: { [TASK]: { ...drafts.get(TASK), text: "Also tighten the abstract", attachments: [attachment] } },
    });
    drafts.chatAbout(TASK, r);
    let draft = drafts.get(TASK);
    expect(draft.text).toBe("Also tighten the abstract");
    expect(draft.attachments).toEqual([attachment]);
    expect(draft.about).toMatchObject({ requestId: r.id, previousText: "Also tighten the abstract" });
    const firstSeq = draft.about!.seq;
    drafts.chatAbout(TASK, r);
    expect(drafts.get(TASK).about!.seq).toBeGreaterThan(firstSeq);

    // The user clears the field to discuss, then sends.
    drafts.setText(TASK, "Could the table use Kelvin but mention Celsius?");
    ipc.agentsQuestionDiscuss.mockResolvedValue(undefined);
    await questions.sendDiscussion(TASK, drafts.get(TASK).about!, "Could the table use Kelvin but mention Celsius?");
    expect(ipc.agentsQuestionDiscuss).toHaveBeenCalledWith(TASK, r.id, 1, "Could the table use Kelvin but mention Celsius?");
    draft = drafts.get(TASK);
    expect(draft.about).toBeNull();
    expect(draft.text).toBe("Also tighten the abstract");
    expect(draft.attachments).toEqual([attachment]);
  });

  it("clearAbout leaves typed text alone and restores the unrelated draft into an empty field", () => {
    const r = request();
    drafts.setText(TASK, "Unrelated");
    drafts.chatAbout(TASK, r);
    drafts.setText(TASK, "half a question");
    drafts.clearAbout(TASK);
    expect(drafts.get(TASK)).toMatchObject({ about: null, text: "half a question" });
    drafts.chatAbout(TASK, r);
    drafts.setText(TASK, "");
    drafts.clearAbout(TASK);
    expect(drafts.get(TASK)).toMatchObject({ about: null, text: "half a question" });
  });

  it("sendDiscussion uses the current revision, retries a stale one once, and clears the About when resolved", async () => {
    const r = request({ revision: 4 });
    put([r]);
    drafts.chatAbout(TASK, r);
    const revised = { ...r, revision: 5 };
    ipc.agentsQuestionDiscuss.mockRejectedValueOnce({ code: "agents.questionStale", message: "stale" }).mockResolvedValueOnce(undefined);
    ipc.agentsTaskDetail.mockResolvedValue(detail([revised]));
    await questions.sendDiscussion(TASK, drafts.get(TASK).about!, "Why Kelvin?");
    expect(ipc.agentsQuestionDiscuss.mock.calls.map((c) => c[2])).toEqual([4, 5]);

    drafts.chatAbout(TASK, revised);
    ipc.agentsQuestionDiscuss.mockRejectedValueOnce({ code: "agents.questionResolved", message: "resolved" });
    await expect(questions.sendDiscussion(TASK, drafts.get(TASK).about!, "And eV?")).rejects.toThrow(
      "This question is no longer open.",
    );
    expect(drafts.get(TASK).about).toBeNull();

    drafts.chatAbout(TASK, revised);
    ipc.agentsQuestionDiscuss.mockRejectedValueOnce({ code: "agents.questionBusy", message: "busy" });
    await expect(questions.sendDiscussion(TASK, drafts.get(TASK).about!, "And eV?")).rejects.toThrow(/still replying/);
    expect(drafts.get(TASK).about).not.toBeNull();
  });
});

describe("question selectors", () => {
  it("map requests by tool call, open first, and label the Needs-you entries", () => {
    const answered = request({ toolUseId: "tu-a", state: { kind: "answered", answers: [] } });
    const open = request({ toolUseId: "tu-b" });
    const discussing = request({ toolUseId: "tu-c", state: { kind: "discussing" } });
    const d = detail([answered, open, discussing]);
    expect([...questions.questionsForTask(d).keys()]).toEqual(["tu-b", "tu-c", "tu-a"]);
    expect(questions.questionsForTask(d).get("tu-a")).toBe(answered);
    expect(questions.pendingQuestions(d)).toEqual([open, discussing]);
    expect(questions.pendingQuestions({ ...d, questions: undefined })).toEqual([]);
    expect(questions.needsYouLabel(open)).toBe("Needs your answer: Which temperature unit should Table 2 use?");
    expect(questions.shortPrompt("Report the activation energy in kJ/mol or eV, given that Section 2 uses kJ/mol?", 40)).toBe(
      "Report the activation energy in kJ/mol…",
    );
  });

  it("jump links focus the card in the same view", () => {
    const r = request();
    render(
      <div data-testid="agents-panel">
        <QuestionCard task={TASK} request={r} />
        <button type="button">Jump</button>
      </div>,
    );
    const link = screen.getByRole("button", { name: "Jump" });
    const scrolled = vi.fn();
    Element.prototype.scrollIntoView = scrolled; // not in jsdom
    expect(questions.jumpToQuestion(r.id, link)).toBe(true);
    expect(document.activeElement).toBe(screen.getByTestId("agents-question"));
    expect(scrolled).toHaveBeenCalledWith({ block: "nearest" });
  });
});

describe("question events and notifications", () => {
  it("ask for an answer persistently and retire when resolved or the task stops", () => {
    const r = request();
    const state = { ...initialAgentsState, tasks: [summary()] };
    const requested = reduceEvent(state, { kind: "questionRequested", request: r });
    expect(requested.refetch).toBe(TASK);
    expect(requested.notification).toMatchObject({
      id: questionNotificationId(TASK),
      tone: "warning",
      title: "Agent needs your answer",
      body: "Which temperature unit should Table 2 use? · Task: Fit the Arrhenius model",
      timeoutMs: null,
    });
    expect(requested.notification?.actions?.map((a) => a.label)).toEqual(["Open question"]);
    expect(reduceEvent(state, { kind: "questionChanged", taskId: TASK, requestId: r.id })).toEqual({ patch: {}, refetch: TASK });
    expect(reduceEvent(state, { kind: "questionResolved", taskId: TASK, requestId: r.id }).dismiss).toEqual([
      questionNotificationId(TASK),
    ]);
    const canceled = reduceEvent(state, { kind: "taskChanged", task: summary({ status: { kind: "canceled" } }) });
    expect(canceled.dismiss).toContain(questionNotificationId(TASK));
    const waiting = reduceEvent(state, { kind: "taskChanged", task: summary() });
    expect(waiting.dismiss).toBeUndefined();
  });

  it("keep a discussion's streamed text while the task waits for the answer", () => {
    const state = {
      ...initialAgentsState,
      drafts: { [TASK]: { turn: 2, text: "Kelvin keeps", thinking: "", activeTool: null } },
    };
    const merged = store.mergeDetail(state, detail([request({ state: { kind: "discussing" } })]));
    expect(merged.drafts?.[TASK]?.text).toBe("Kelvin keeps");
  });
});

describe("Waiting for your answer", () => {
  beforeEach(() => layout.setVisible("auxiliary", false));

  it("is the warning status label and counts as needing the user", () => {
    expect(format.statusLabel({ kind: "awaitingAnswer" })).toEqual({ status: "warning", label: "Waiting for your answer" });
    expect(format.statusLabel({ kind: "awaitingApproval" })).toEqual({ status: "warning", label: "Waiting for approval" });
    expect(format.isActive({ kind: "awaitingAnswer" })).toBe(true);
    expect(format.needsUser({ kind: "awaitingAnswer" })).toBe(true);
    expect(format.needsUser({ kind: "awaitingApproval" })).toBe(true);
    expect(format.needsUser({ kind: "running" })).toBe(false);
    expect(format.attentionLabel(1, 0)).toBe("Agent: waiting for approval");
    expect(format.attentionLabel(2, 0)).toBe("Agent: 2 waiting for approval");
    expect(format.attentionLabel(0, 1)).toBe("Agent: waiting for your answer");
    expect(format.attentionLabel(0, 3)).toBe("Agent: 3 waiting for your answer");
    expect(format.attentionLabel(1, 1)).toBe("Agent: 2 waiting for you");
  });

  it("asks for attention in the status bar and opens the question", () => {
    useAgents.setState({ tasks: [summary(), summary({ id: "task-2", status: { kind: "running" } })] });
    render(<AgentStatusItem />);
    const button = screen.getByRole("button", { name: "Agent: waiting for your answer" });
    expect(button.querySelector("[data-status='warning']")).toBeTruthy();
    expect(button.getAttribute("title")).toBe("Open the pending question");
    act(() => {
      fireEvent.click(button);
    });
    expect(layout.isVisible("auxiliary")).toBe(true);
    expect(useAgents.getState()).toMatchObject({ view: "task", selected: TASK });
  });

  it("counts approvals and questions together, keeping the approval wording when only approvals wait", () => {
    useAgents.setState({
      tasks: [summary({ status: { kind: "awaitingApproval" } }), summary({ id: "task-2", status: { kind: "awaitingAnswer" } })],
    });
    const { rerender } = render(<AgentStatusItem />);
    expect(screen.getByRole("button", { name: "Agent: 2 waiting for you" })).toBeTruthy();
    useAgents.setState({ tasks: [summary({ status: { kind: "awaitingApproval" } })] });
    rerender(<AgentStatusItem />);
    expect(screen.getByRole("button", { name: "Agent: waiting for approval" })).toBeTruthy();
  });
});
