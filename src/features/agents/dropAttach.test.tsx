// Files dropped from the operating system on the Agent chat, through the
// real native-drop router (`workspace/dropTarget.ts`): one target per drop
// (the chat or the Files view, never both), the highlight and its clearing,
// folders and unreadable files explained beside what was attached, the same
// content never twice, the per-message limit, results bound to the draft the
// files were dropped on, and nothing sent or started.
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { AttachmentInfo } from "../../bindings/AttachmentInfo";
import type { IngestOutcome } from "../../bindings/IngestOutcome";
import type { ProviderStatus } from "../../bindings/ProviderStatus";
import type { TaskSummary } from "../../bindings/TaskSummary";

const ipc = vi.hoisted(() => ({
  agentsAttachmentsPick: vi.fn(),
  agentsAttachmentsAddBytes: vi.fn(),
  agentsAttachmentsAddDropped: vi.fn(),
  agentsAttachmentPreview: vi.fn(),
  agentsAttachmentsDiscard: vi.fn(),
  agentsTaskCreate: vi.fn(),
  agentsTaskContinue: vi.fn(),
  agentsModelsList: vi.fn(),
}));
vi.mock("../../lib/ipc", () => ({
  commands: ipc,
  events: { agents: vi.fn(), agentsResync: vi.fn() },
  errorMessage: (e: unknown) => (e instanceof Error ? e.message : String(e)),
}));
vi.mock("./ModelMenu", () => ({ ModelMenu: () => <div data-testid="model-menu" /> }));
// The Files view's side of the router: its import and its open project.
const files = vi.hoisted(() => ({ startImport: vi.fn() }));
vi.mock("../workspace/explorerActions", () => ({ startImport: files.startImport }));
vi.mock("../workspace/store", async () => {
  const { create } = await import("zustand");
  return { useWorkspace: create(() => ({ info: { root: "C:/paper", name: "paper" } })) };
});

const { handleNativeDrop, useDropTarget } = await import("../workspace/dropTarget");
const { Composer, TaskComposer, DEFAULT_BUDGETS, resetComposerForTests } = await import("./TaskComposer");
const { agentDrop, composerAt, startAgentDrop, useAgentDrop } = await import("./dropAttach");
const { drafts, NEW_TASK_DRAFT, MAX_ATTACHMENTS_PER_MESSAGE, MAX_DROP_FILES } = await import("./drafts");
const { initialAgentsState, useAgents } = await import("./store");

let nextId = 0;
function info(name: string, overrides: Partial<AttachmentInfo> = {}): AttachmentInfo {
  const n = ++nextId;
  return {
    id: `att-${n}`,
    name,
    mediaType: "text/plain",
    kind: "text",
    sizeBytes: 812,
    width: null,
    height: null,
    sha256: n.toString(16).padStart(64, "0"),
    missing: false,
    ...overrides,
  };
}
const added = (attachment: AttachmentInfo): IngestOutcome => ({ kind: "added", attachment });
const rejected = (name: string, code: string, message: string): IngestOutcome => ({ kind: "rejected", name, code, message });

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

const status: ProviderStatus = {
  settings: { provider: "anthropic", model: "claude-opus-5-5", effort: "medium", consent: "scopeContent" },
  configured: true,
  connections: [{ provider: "anthropic", auth: "apiKey", configured: true, credentialError: null, context: null, version: 1 }],
  credentialStore: "test store",
  credentialError: null,
  scriptedAvailable: false,
};

function summary(id: string, overrides: Partial<TaskSummary> = {}): TaskSummary {
  return {
    id,
    title: `Task ${id}`,
    createdAtMs: 1,
    updatedAtMs: 1,
    status: { kind: "completed" },
    spec: {
      prompt: "Describe the figure",
      workspaceRoot: "C:/paper",
      scope: { kind: "workspace" },
      permission: "reviewEdits",
      execution: "disabled",
      consent: "scopeContent",
      budgets: DEFAULT_BUDGETS,
      provider: { provider: "anthropic", model: "claude-opus-5-5", effort: "medium" },
      approvalMode: "ask",
    },
    usage: { turns: 1, inputTokens: 1, outputTokens: 1, cacheReadTokens: 0, cacheWriteTokens: 0, toolCalls: 0, commands: 0, elapsedMs: 0, servedBy: null },
    pendingReview: 0,
    changedFiles: 0,
    ...overrides,
  };
}

/** The app as the router sees it: an editor, the Files view, and a chat (transcript + composer). */
function App({ task = null }: { task?: TaskSummary | null }) {
  return (
    <>
      <div data-testid="editor">editor</div>
      <div data-testid="files-view">
        <div data-testid="files-surface">
          <div data-path="figures" data-kind="directory" data-testid="folder-row">
            figures
          </div>
        </div>
      </div>
      <section data-agent-chat data-testid="chat">
        <div data-testid="transcript">transcript</div>
        {task ? <Composer key={task.id} task={task} /> : <TaskComposer onCreated={() => {}} />}
      </section>
    </>
  );
}

const composer = () => screen.getByTestId("agents-composer");
const highlighted = () => composer().hasAttribute("data-drop-target");
const chips = () => screen.queryAllByTestId("agents-attachment").map((r) => r.querySelector(".ui-chip__label")?.textContent ?? "");
const rejections = () => screen.queryAllByTestId("agents-attachment-rejection").map((l) => l.textContent);
let under: Element | null = null;
/** A native event with the pointer over `element`. */
function native(type: "enter" | "over" | "drop" | "leave", element: Element | null, paths?: string[]) {
  under = element;
  act(() => handleNativeDrop(type === "leave" ? { type } : { type, paths, position: { x: 10, y: 10 } }, 1));
}

beforeEach(() => {
  for (const fn of Object.values(ipc)) fn.mockReset();
  files.startImport.mockReset();
  ipc.agentsAttachmentPreview.mockResolvedValue(null);
  ipc.agentsAttachmentsDiscard.mockResolvedValue(undefined);
  drafts.resetForTests();
  resetComposerForTests();
  useAgentDrop.setState({ zone: null });
  useDropTarget.setState({ target: undefined });
  useAgents.setState({ ...initialAgentsState, status, workspace: { root: "C:/paper", name: "paper" } as never, tasks: [summary("task-a"), summary("task-b")] });
  document.elementFromPoint = () => under;
  startAgentDrop();
});
afterEach(() => {
  cleanup();
  under = null;
});

describe("the chat as a drop target", () => {
  it("is the composer under the pointer or the composer of the chat region under it, and nothing else", () => {
    render(<App />);
    const zone = composer().getAttribute("data-agent-drop-zone");
    expect(zone).toBeTruthy();
    expect(composerAt(screen.getByTestId("agents-prompt"))).toEqual({ zone, draftKey: NEW_TASK_DRAFT });
    expect(composerAt(screen.getByTestId("transcript"))).toEqual({ zone, draftKey: NEW_TASK_DRAFT });
    expect(composerAt(screen.getByTestId("editor"))).toBeNull();
    expect(composerAt(screen.getByTestId("folder-row"))).toBeNull();
    expect(composerAt(null)).toBeNull();
  });

  it("highlights only while a drag is over it, and clears on leave, cancel, another target, and when it goes away", () => {
    const view = render(<App />);
    expect(highlighted()).toBe(false);
    native("enter", screen.getByTestId("transcript"), ["C:/in/a.txt"]);
    expect(highlighted()).toBe(true);
    // The Files view is not also a target for this drag.
    expect(useDropTarget.getState().target).toBeUndefined();
    native("over", screen.getByTestId("editor"));
    expect(highlighted()).toBe(false);
    native("over", screen.getByTestId("agents-prompt"));
    expect(highlighted()).toBe(true);
    // Over the Files view: its folder is the target, the chat is not.
    native("over", screen.getByTestId("folder-row"));
    expect(highlighted()).toBe(false);
    expect(useDropTarget.getState().target).toEqual({ folder: "figures" });
    native("over", screen.getByTestId("agents-prompt"));
    expect(useDropTarget.getState().target).toBeUndefined();
    expect(highlighted()).toBe(true);
    // The drag leaves the window or is canceled.
    native("leave", null);
    expect(highlighted()).toBe(false);
    // The conversation changes during a drag: the old composer's highlight goes with it.
    native("over", screen.getByTestId("agents-prompt"));
    expect(useAgentDrop.getState().zone).not.toBeNull();
    view.rerender(<App task={summary("task-a")} />);
    expect(useAgentDrop.getState().zone).toBeNull();
    expect(highlighted()).toBe(false);
    expect(ipc.agentsAttachmentsAddDropped).not.toHaveBeenCalled();
  });

  it("attaches a drop to the chat's draft only: no project import, nothing sent, the highlight gone", async () => {
    const a = info("notes.md", { mediaType: "text/markdown" });
    const b = info("plot.png", { kind: "image", mediaType: "image/png", width: 4, height: 3 });
    ipc.agentsAttachmentsAddDropped.mockResolvedValue([added(a), added(b)]);
    render(<App />);
    native("enter", screen.getByTestId("transcript"), ["C:\\in\\notes.md", "C:\\in\\plot.png"]);
    native("drop", screen.getByTestId("transcript"), ["C:\\in\\notes.md", "C:\\in\\plot.png"]);
    expect(highlighted()).toBe(false);
    await waitFor(() => expect(chips()).toEqual(["notes.md", "plot.png"]));
    expect(ipc.agentsAttachmentsAddDropped).toHaveBeenCalledTimes(1);
    expect(ipc.agentsAttachmentsAddDropped).toHaveBeenCalledWith(["C:\\in\\notes.md", "C:\\in\\plot.png"]);
    expect(files.startImport).not.toHaveBeenCalled();
    expect(ipc.agentsTaskCreate).not.toHaveBeenCalled();
    expect(ipc.agentsTaskContinue).not.toHaveBeenCalled();
    // No path is shown anywhere.
    expect(composer().textContent).not.toContain("C:\\in");
    // The Attach button is still there for the keyboard.
    expect(screen.getByRole("button", { name: "Attach files…" })).toBeTruthy();
  });

  it("leaves a drop on the Files view to the project import, and a drop elsewhere to nobody", async () => {
    render(<App />);
    native("drop", screen.getByTestId("folder-row"), ["C:/in/a.png"]);
    expect(files.startImport).toHaveBeenCalledWith(["C:/in/a.png"], "figures");
    native("drop", screen.getByTestId("editor"), ["C:/in/b.png"]);
    expect(files.startImport).toHaveBeenCalledTimes(1);
    await new Promise((r) => setTimeout(r, 10));
    expect(ipc.agentsAttachmentsAddDropped).not.toHaveBeenCalled();
    expect(chips()).toEqual([]);
  });

  it("explains a folder and an unreadable file beside the files that were attached", async () => {
    ipc.agentsAttachmentsAddDropped.mockResolvedValue([
      added(info("data.csv", { mediaType: "text/csv" })),
      rejected("figures", "attachments.folder", "Folders cannot be attached. Attach the files inside it instead."),
      rejected("locked.txt", "attachments.unreadable", "The file could not be read: Access is denied."),
      rejected("tool.exe", "attachments.unsupported", "This kind of file cannot be attached."),
    ]);
    render(<App />);
    native("drop", screen.getByTestId("agents-prompt"), ["C:/in/data.csv", "C:/in/figures", "C:/in/locked.txt", "C:/in/tool.exe"]);
    await waitFor(() => expect(chips()).toEqual(["data.csv"]));
    expect(screen.getByTestId("agents-attachment-rejections").textContent).toContain("3 files were not attached");
    expect(rejections()).toEqual([
      "figures: Folders cannot be attached. Attach the files inside it instead.",
      "locked.txt: The file could not be read: Access is denied.",
      "tool.exe: This kind of file cannot be attached.",
    ]);
  });

  it("does not attach the same content twice", async () => {
    const first = info("notes.md");
    const again = info("notes.md", { sha256: first.sha256 });
    const copy = info("notes copy.md", { sha256: first.sha256 });
    const other = info("other.md");
    ipc.agentsAttachmentsAddDropped.mockResolvedValueOnce([added(first)]).mockResolvedValueOnce([added(again), added(copy), added(other), added(info("twin.md", { sha256: other.sha256 }))]);
    render(<App />);
    native("drop", screen.getByTestId("agents-prompt"), ["C:/in/notes.md"]);
    await waitFor(() => expect(chips()).toEqual(["notes.md"]));
    native("drop", screen.getByTestId("agents-prompt"), ["C:/in/notes.md", "C:/in/notes copy.md", "C:/in/other.md", "C:/in/twin.md"]);
    await waitFor(() => expect(chips()).toEqual(["notes.md", "other.md"]));
    expect(rejections()).toEqual([
      "notes.md: Already attached.",
      "notes copy.md: Already attached as “notes.md” (the same content).",
      "twin.md: Already attached as “other.md” (the same content).",
    ]);
    // The unused snapshots are forgotten.
    await waitFor(() => expect(ipc.agentsAttachmentsDiscard).toHaveBeenCalledWith([again.id, copy.id, expect.any(String)]));
  });

  it("keeps to the per-message limit and to the files of one drop, saying what was left out", async () => {
    const dropped = Array.from({ length: MAX_ATTACHMENTS_PER_MESSAGE + 2 }, (_, i) => info(`f${i}.txt`));
    ipc.agentsAttachmentsAddDropped.mockResolvedValueOnce(dropped.map(added));
    render(<App />);
    native("drop", screen.getByTestId("agents-prompt"), dropped.map((d) => `C:/in/${d.name}`));
    await waitFor(() => expect(chips()).toHaveLength(MAX_ATTACHMENTS_PER_MESSAGE));
    expect(rejections()).toEqual([
      "f20.txt: A message can carry at most 20 attachments. Remove one to attach this file.",
      "f21.txt: A message can carry at most 20 attachments. Remove one to attach this file.",
    ]);
    await waitFor(() => expect(ipc.agentsAttachmentsDiscard).toHaveBeenCalledWith([dropped[20]!.id, dropped[21]!.id]));

    // More files than one drop takes: the first ones are read, the rest are named as left out.
    act(() => drafts.resetForTests());
    ipc.agentsAttachmentsAddDropped.mockResolvedValueOnce([]);
    const many = Array.from({ length: MAX_DROP_FILES + 3 }, (_, i) => `C:/in/m${i}.txt`);
    native("drop", screen.getByTestId("agents-prompt"), many);
    await waitFor(() => expect(rejections()).toEqual(["3 more files: At most 100 files can be dropped at once."]));
    expect(ipc.agentsAttachmentsAddDropped.mock.calls[1]![0]).toEqual(many.slice(0, MAX_DROP_FILES));
  });

  it("says so when the files could not be read at all, and keeps the draft", async () => {
    ipc.agentsAttachmentsAddDropped.mockRejectedValue(new Error("At most 100 files can be dropped at once."));
    render(<App />);
    act(() => drafts.setText(NEW_TASK_DRAFT, "Keep me"));
    native("drop", screen.getByTestId("agents-prompt"), ["C:/in/a.txt"]);
    await waitFor(() => expect(rejections()).toEqual(["Dropped files: At most 100 files can be dropped at once."]));
    expect((screen.getByTestId("agents-prompt") as HTMLTextAreaElement).value).toBe("Keep me");
  });
});

describe("a drop is bound to the conversation it was dropped on", () => {
  it("lands in that task's draft after switching to another task, and Send waits while it is read", async () => {
    const reading = deferred<IngestOutcome[]>();
    ipc.agentsAttachmentsAddDropped.mockReturnValue(reading.promise);
    const view = render(<App task={summary("task-a")} />);
    native("drop", screen.getByTestId("transcript"), ["C:/in/a.txt"]);
    expect(screen.getByText("Adding attachments…")).toBeTruthy();
    view.rerender(<App task={summary("task-b")} />);
    expect(screen.queryByText("Adding attachments…")).toBeNull();
    const a = info("a.txt");
    await act(async () => reading.resolve([added(a)]));
    expect(chips()).toEqual([]);
    expect(drafts.get("task-b").attachments).toEqual([]);
    expect(drafts.get("task-a").attachments).toEqual([a]);
    view.rerender(<App task={summary("task-a")} />);
    expect(chips()).toEqual(["a.txt"]);
    expect(ipc.agentsTaskContinue).not.toHaveBeenCalled();
  });

  it("is discarded when the project changed or the task is gone before the files were read", async () => {
    // The new-task draft belongs to the project that was open at the drop.
    const reading = deferred<IngestOutcome[]>();
    ipc.agentsAttachmentsAddDropped.mockReturnValueOnce(reading.promise);
    const view = render(<App />);
    native("drop", screen.getByTestId("agents-prompt"), ["C:/in/a.txt"]);
    act(() => useAgents.setState({ workspace: { root: "C:/other", name: "other" } as never }));
    const a = info("a.txt");
    await act(async () => reading.resolve([added(a), rejected("b.bin", "attachments.unsupported", "This kind of file cannot be attached.")]));
    expect(chips()).toEqual([]);
    expect(rejections()).toEqual([]);
    expect(drafts.get(NEW_TASK_DRAFT).attachments).toEqual([]);
    expect(drafts.get(NEW_TASK_DRAFT).ingesting).toBe(0);
    await waitFor(() => expect(ipc.agentsAttachmentsDiscard).toHaveBeenCalledWith([a.id]));

    // A task that was deleted meanwhile.
    const second = deferred<IngestOutcome[]>();
    ipc.agentsAttachmentsAddDropped.mockReturnValueOnce(second.promise);
    view.rerender(<App task={summary("task-a")} />);
    native("drop", screen.getByTestId("agents-follow-up-text"), ["C:/in/c.txt"]);
    act(() => useAgents.setState({ tasks: [summary("task-b")] }));
    const c = info("c.txt");
    await act(async () => second.resolve([added(c)]));
    expect(drafts.get("task-a").attachments).toEqual([]);
    await waitFor(() => expect(ipc.agentsAttachmentsDiscard).toHaveBeenCalledWith([c.id]));
  });

  it("a zone that does not own the point claims nothing", () => {
    render(<App />);
    expect(agentDrop.hover(screen.getByTestId("editor"))).toBe(false);
    expect(agentDrop.drop(screen.getByTestId("editor"), ["C:/in/a.txt"])).toBe(false);
    expect(ipc.agentsAttachmentsAddDropped).not.toHaveBeenCalled();
  });
});
