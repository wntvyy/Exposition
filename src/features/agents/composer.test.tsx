// Composer attachments and model choice: attach (partial rejection kept with
// the files that were added), remove, paste images (named, repeated, several,
// one image offered twice), text and mixed pastes, sending blocked while
// ingesting, a failed send keeps the draft, success clears only what was sent,
// drafts per conversation, follow-up model switches (and the backend's
// refusal), and attachments in the transcript.
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { AttachmentInfo } from "../../bindings/AttachmentInfo";
import type { IngestOutcome } from "../../bindings/IngestOutcome";
import type { ModelCatalog } from "../../bindings/ModelCatalog";
import type { ProviderStatus } from "../../bindings/ProviderStatus";
import type { TaskDetail } from "../../bindings/TaskDetail";
import type { TaskSummary } from "../../bindings/TaskSummary";

const ipc = vi.hoisted(() => ({
  agentsAttachmentsPick: vi.fn(),
  agentsAttachmentsAddBytes: vi.fn(),
  agentsAttachmentPreview: vi.fn(),
  agentsAttachmentsDiscard: vi.fn(),
  agentsTaskCreate: vi.fn(),
  agentsTaskContinue: vi.fn(),
  agentsTaskDetail: vi.fn(),
  agentsModelsList: vi.fn(),
}));
vi.mock("../../lib/ipc", () => ({
  commands: ipc,
  events: { agents: vi.fn(), agentsResync: vi.fn() },
  errorMessage: (e: unknown) => (e instanceof Error ? e.message : String(e)),
}));
// The model menu belongs to the settings lane; the composer only passes its
// value, the running model, and the provider-settings action through.
const modelMenu = vi.hoisted(() => ({ props: null as null | Record<string, unknown> }));
vi.mock("./ModelMenu", () => ({
  ModelMenu: (props: { value: unknown; onChange: (c: unknown) => void; running?: unknown; onManageProviders: () => void }) => {
    modelMenu.props = props;
    return (
      <div data-testid="model-menu" data-value={JSON.stringify(props.value)} data-running={JSON.stringify(props.running ?? null)}>
        <button type="button" onClick={() => props.onChange({ provider: "openai", model: "gpt-6.2-sol", effort: "medium" })}>
          Pick gpt-6.2-sol
        </button>
        <button type="button" onClick={() => props.onChange({ provider: "openai", model: "gpt-4.1-nano", effort: "medium" })}>
          Pick gpt-4.1-nano
        </button>
        <button type="button" onClick={() => props.onChange(null)}>
          Inherit
        </button>
      </div>
    );
  },
}));
const registryRun = vi.hoisted(() => vi.fn());
vi.mock("../../app/registry", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../app/registry")>();
  return { ...actual, registry: { ...actual.registry, runCommand: registryRun } };
});

const { TaskComposer, DEFAULT_BUDGETS, resetComposerForTests } = await import("./TaskComposer");
const { TaskView } = await import("./TaskView");
const { TranscriptEntry } = await import("./Transcript");
const { drafts, NEW_TASK_DRAFT, clipboardImages, genericPasteName } = await import("./drafts");
const { attachmentMeta } = await import("./Attachments");
const { initialAgentsState, useAgents } = await import("./store");

let nextId = 0;
function info(name: string, overrides: Partial<AttachmentInfo> = {}): AttachmentInfo {
  return {
    id: `att-${++nextId}`,
    name,
    mediaType: "text/plain",
    kind: "text",
    sizeBytes: 812,
    width: null,
    height: null,
    sha256: "0".repeat(64),
    missing: false,
    ...overrides,
  };
}
const added = (attachment: AttachmentInfo): IngestOutcome => ({ kind: "added", attachment });
const image = (name: string) => info(name, { kind: "image", mediaType: "image/png", sizeBytes: 49_357, width: 1280, height: 720 });

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const status: ProviderStatus = {
  settings: { provider: "anthropic", model: "claude-opus-5-5", effort: "medium", consent: "scopeContent" },
  configured: true,
  connections: [
    { provider: "anthropic", auth: "apiKey", configured: true, credentialError: null, context: null, version: 1 },
    { provider: "openai", auth: "apiKey", configured: true, credentialError: null, context: null, version: 1 },
  ],
  credentialStore: "test store",
  credentialError: null,
  scriptedAvailable: false,
};

function summary(overrides: Partial<TaskSummary> = {}): TaskSummary {
  return {
    id: "task-1",
    title: "Describe the figure",
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

function detail(task: TaskSummary): TaskDetail {
  return {
    summary: task,
    transcript: [],
    approvals: [],
    graph: { project: null, context: null, contexts: 0, targets: [], proposals: [], investigation: null },
    allowances: [],
  };
}

/** A paste event's clipboard: string items and file items (as WebView2 offers them). */
function clipboard({ text, html, uri, files = [], filesAgain = true }: { text?: string; html?: string; uri?: string; files?: File[]; filesAgain?: boolean }) {
  const items = [
    ...(text !== undefined ? [{ kind: "string", type: "text/plain", getAsFile: () => null }] : []),
    ...(html !== undefined ? [{ kind: "string", type: "text/html", getAsFile: () => null }] : []),
    ...(uri !== undefined ? [{ kind: "string", type: "text/uri-list", getAsFile: () => null }] : []),
    ...files.map((file) => ({ kind: "file", type: file.type, getAsFile: () => file })),
  ];
  return { items, files: filesAgain ? files : [], types: items.map((i) => i.type), getData: () => text ?? "" };
}

const png = (bytes: number[], name = "image.png") => new File([new Uint8Array(bytes)], name, { type: "image/png" });
const start = () => screen.getByTestId("agents-start") as HTMLButtonElement;

beforeEach(() => {
  for (const fn of Object.values(ipc)) fn.mockReset();
  ipc.agentsAttachmentPreview.mockResolvedValue("data:image/png;base64,iVBORw0KGgo=");
  ipc.agentsAttachmentsDiscard.mockResolvedValue(undefined);
  ipc.agentsAttachmentsAddBytes.mockImplementation((name: string, mediaType: string) =>
    Promise.resolve(added(info(name, { kind: "image", mediaType, sizeBytes: 3, width: 4, height: 3 }))),
  );
  ipc.agentsTaskCreate.mockResolvedValue(summary({ id: "task-9" }));
  drafts.resetForTests();
  resetComposerForTests();
  registryRun.mockReset();
  useAgents.setState({ ...initialAgentsState, status, workspace: { root: "C:/paper", name: "paper" } as never });
});
afterEach(cleanup);

/** Open the composer's Options sheet on a tab. */
function openOptions(tab: "Model" | "Permissions" | "Budgets") {
  const button = screen.getByTestId("agents-options");
  if (button.getAttribute("aria-expanded") !== "true") fireEvent.click(button);
  fireEvent.click(screen.getByRole("tab", { name: new RegExp(`^${tab}`) }));
}
const optionsText = () => screen.getByTestId("agents-options").textContent ?? "";

describe("clipboard and formatting helpers", () => {
  it("takes image files once, from the items when they hold any, and never text, HTML, or URLs", () => {
    const a = png([1, 2, 3]);
    expect(clipboardImages(clipboard({ files: [a] }) as never)).toEqual([a]);
    expect(clipboardImages(clipboard({ files: [a], filesAgain: false }) as never)).toEqual([a]);
    expect(clipboardImages({ items: [], files: [a] } as never)).toEqual([a]);
    expect(clipboardImages(clipboard({ text: "x", html: '<img src="https://example.com/a.png">', uri: "https://example.com/a.png" }) as never)).toEqual([]);
    expect(clipboardImages(null)).toEqual([]);
    expect(genericPasteName("image.png")).toBe(true);
    expect(genericPasteName("")).toBe(true);
    expect(genericPasteName("chart.png")).toBe(false);
    expect(attachmentMeta(info("notes.txt", { kind: "text", mediaType: "text/plain", sizeBytes: 812 }))).toBe("Text · 812 B");
    expect(attachmentMeta(image("a.png"))).toBe("PNG image · 48.2 KiB · 1280×720");
    expect(attachmentMeta(info("paper.pdf", { kind: "pdf", mediaType: "application/pdf", sizeBytes: 1_200_000 }))).toBe("PDF · 1.1 MiB");
  });
});

describe("new task composer", () => {
  it("attaches files, keeps them when others are rejected, keeps duplicates distinct, and removes one", async () => {
    const first = info("notes.md", { mediaType: "text/markdown" });
    const second = info("notes.md", { mediaType: "text/markdown" });
    ipc.agentsAttachmentsPick.mockResolvedValue([
      added(first),
      { kind: "rejected", name: "tool.exe", code: "attachments.unsupported", message: "This kind of file cannot be attached." },
      added(second),
    ]);
    render(<TaskComposer onCreated={() => {}} />);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Attach files…" }));
    });
    const rows = within(screen.getByRole("list", { name: "Attachments" })).getAllByTestId("agents-attachment");
    expect(rows.map((r) => r.dataset.attachmentId)).toEqual([first.id, second.id]);
    expect(rows[0]!.textContent).toContain("Text · 812 B");
    const rejections = screen.getByTestId("agents-attachment-rejections");
    expect(rejections.textContent).toContain("A file was not attached");
    expect(rejections.textContent).toContain("tool.exe: This kind of file cannot be attached.");

    fireEvent.click(screen.getAllByRole("button", { name: "Remove notes.md" })[0]!);
    await waitFor(() => expect(ipc.agentsAttachmentsDiscard).toHaveBeenCalledWith([first.id]));
    expect(screen.getAllByTestId("agents-attachment").map((r) => r.dataset.attachmentId)).toEqual([second.id]);
    fireEvent.click(within(rejections).getByRole("button", { name: "Dismiss" }));
    expect(screen.queryByTestId("agents-attachment-rejections")).toBeNull();
  });

  it("cannot start while attachments are being added; an attachment-only message can start", async () => {
    const pick = deferred<IngestOutcome[]>();
    ipc.agentsAttachmentsPick.mockReturnValue(pick.promise);
    render(<TaskComposer onCreated={() => {}} />);
    expect(start().disabled).toBe(true);
    act(() => {
      fireEvent.click(screen.getByRole("button", { name: "Attach files…" }));
    });
    expect(screen.getByText("Adding attachments…")).toBeTruthy();
    fireEvent.change(screen.getByTestId("agents-prompt"), { target: { value: "Summarize" } });
    expect(start().disabled).toBe(true);
    const doc = info("data.csv", { mediaType: "text/csv" });
    await act(async () => pick.resolve([added(doc)]));
    expect(start().disabled).toBe(false);
    fireEvent.change(screen.getByTestId("agents-prompt"), { target: { value: "" } });
    expect(start().disabled).toBe(false);
    await act(async () => {
      fireEvent.click(start());
    });
    expect(ipc.agentsTaskCreate).toHaveBeenCalledWith(expect.objectContaining({ prompt: "", attachments: [doc.id] }));
    expect(ipc.agentsTaskCreate.mock.calls[0]![0]).not.toHaveProperty("provider");
  });

  it("turns pasted images into named attachments with thumbnails, once per image, and leaves text pastes alone", async () => {
    render(<TaskComposer onCreated={() => {}} />);
    const field = screen.getByTestId("agents-prompt");
    // A screenshot: one image offered as a file item and in the file list.
    const shot = png([137, 80, 78, 71, 1]);
    expect(fireEvent.paste(field, { clipboardData: clipboard({ files: [shot] }) })).toBe(true);
    await waitFor(() => expect(ipc.agentsAttachmentsAddBytes).toHaveBeenCalledTimes(1));
    expect(ipc.agentsAttachmentsAddBytes).toHaveBeenCalledWith("Pasted image 1.png", "image/png", btoa(String.fromCharCode(137, 80, 78, 71, 1)));
    // Repeated paste of the same screenshot: a second, distinct attachment.
    fireEvent.paste(field, { clipboardData: clipboard({ files: [shot] }) });
    await waitFor(() => expect(ipc.agentsAttachmentsAddBytes).toHaveBeenCalledTimes(2));
    expect(ipc.agentsAttachmentsAddBytes.mock.calls[1]![0]).toBe("Pasted image 2.png");
    // Several images in one paste (a named file keeps its name); the same bytes twice count once.
    const chart = png([1, 1, 1], "chart.png");
    fireEvent.paste(field, { clipboardData: clipboard({ files: [chart, png([2, 2]), png([2, 2])], filesAgain: false }) });
    await waitFor(() => expect(ipc.agentsAttachmentsAddBytes).toHaveBeenCalledTimes(4));
    expect(ipc.agentsAttachmentsAddBytes.mock.calls.slice(2).map((c) => c[0])).toEqual(["chart.png", "Pasted image 3.png"]);
    await waitFor(() => expect(screen.getAllByTestId("agents-attachment-thumb")).toHaveLength(4));
    expect(screen.getAllByTestId("agents-attachment").map((r) => r.textContent?.slice(0, 18))).toEqual([
      "Pasted image 1.png",
      "Pasted image 2.png",
      "chart.pngPNG image",
      "Pasted image 3.png",
    ]);

    // Plain text, HTML with an image reference, and a URL: no attachment, no fetch, not intercepted.
    ipc.agentsAttachmentsAddBytes.mockClear();
    expect(fireEvent.paste(field, { clipboardData: clipboard({ text: "plain words" }) })).toBe(true);
    expect(fireEvent.paste(field, { clipboardData: clipboard({ text: "a", html: '<img src="https://example.com/x.png">', uri: "https://example.com/x.png" }) })).toBe(true);
    // Mixed text and image: the text goes to the field (not prevented) and the image is attached.
    expect(fireEvent.paste(field, { clipboardData: clipboard({ text: "caption", files: [png([9])] }) })).toBe(true);
    await waitFor(() => expect(ipc.agentsAttachmentsAddBytes).toHaveBeenCalledTimes(1));
    expect(ipc.agentsAttachmentsAddBytes.mock.calls[0]![0]).toBe("Pasted image 4.png");
  });

  it("explains a rejected pasted image and keeps the other attachments", async () => {
    ipc.agentsAttachmentsAddBytes
      .mockResolvedValueOnce(added(image("Pasted image 1.png")))
      .mockResolvedValueOnce({ kind: "rejected", name: "Pasted image 2.png", code: "attachments.imageTooLarge", message: "The image is 9.0 MiB; images can be at most 5.0 MiB." });
    render(<TaskComposer onCreated={() => {}} />);
    fireEvent.paste(screen.getByTestId("agents-prompt"), { clipboardData: clipboard({ files: [png([1]), png([2])], filesAgain: false }) });
    await screen.findByTestId("agents-attachment-rejections");
    expect(screen.getByTestId("agents-attachment-rejection").textContent).toBe("Pasted image 2.png: The image is 9.0 MiB; images can be at most 5.0 MiB.");
    expect(screen.getAllByTestId("agents-attachment")).toHaveLength(1);
  });

  it("keeps the text and attachments when starting fails, and shows why", async () => {
    const doc = info("notes.txt");
    ipc.agentsAttachmentsPick.mockResolvedValue([added(doc)]);
    ipc.agentsTaskCreate.mockRejectedValue(new Error("Anthropic model claude-x is not known to accept images."));
    render(<TaskComposer onCreated={() => {}} />);
    fireEvent.change(screen.getByTestId("agents-prompt"), { target: { value: "Read these" } });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Attach files…" }));
    });
    await act(async () => {
      fireEvent.click(start());
    });
    expect(screen.getByTestId("agents-composer-error").textContent).toContain("is not known to accept images");
    expect((screen.getByTestId("agents-prompt") as HTMLTextAreaElement).value).toBe("Read these");
    expect(screen.getAllByTestId("agents-attachment").map((r) => r.dataset.attachmentId)).toEqual([doc.id]);
    expect(start().disabled).toBe(false);
  });

  it("after starting, clears only what was sent: text and attachments added meanwhile stay", async () => {
    const created = deferred<TaskSummary>();
    ipc.agentsTaskCreate.mockReturnValue(created.promise);
    const sent = image("Pasted image 1.png");
    const later = image("Pasted image 2.png");
    ipc.agentsAttachmentsAddBytes.mockResolvedValueOnce(added(sent)).mockResolvedValueOnce(added(later));
    const onCreated = vi.fn();
    render(<TaskComposer onCreated={onCreated} />);
    const field = screen.getByTestId("agents-prompt");
    fireEvent.change(field, { target: { value: "Describe the plot" } });
    fireEvent.paste(field, { clipboardData: clipboard({ files: [png([1])] }) });
    await screen.findByTestId("agents-attachment");
    act(() => {
      fireEvent.click(start());
    });
    expect(ipc.agentsTaskCreate).toHaveBeenCalledWith(expect.objectContaining({ prompt: "Describe the plot", attachments: [sent.id] }));
    // While the request runs: more text and another image.
    fireEvent.change(field, { target: { value: "Describe the plot and then the table" } });
    fireEvent.paste(field, { clipboardData: clipboard({ files: [png([2])] }) });
    await waitFor(() => expect(screen.getAllByTestId("agents-attachment")).toHaveLength(2));
    await act(async () => created.resolve(summary({ id: "task-9" })));
    expect(onCreated).toHaveBeenCalledWith("task-9");
    expect((field as HTMLTextAreaElement).value).toBe("and then the table");
    expect(screen.getAllByTestId("agents-attachment").map((r) => r.dataset.attachmentId)).toEqual([later.id]);
  });

  it("inherits the default model unless one is chosen for this task, and sends the explicit choice", async () => {
    render(<TaskComposer onCreated={() => {}} />);
    expect(optionsText()).toBe("Options: claude-opus-5-5 · Medium · Ask · Ask before each change");
    openOptions("Model");
    expect(screen.getByTestId("model-menu").dataset.value).toBe("null");
    expect(screen.getByTestId("model-menu").dataset.running).toBe("null");
    fireEvent.click(screen.getByRole("button", { name: "Pick gpt-6.2-sol" }));
    expect(optionsText()).toBe("Options: gpt-6.2-sol · Medium · Ask · Ask before each change");
    fireEvent.change(screen.getByTestId("agents-prompt"), { target: { value: "Fit the model" } });
    await act(async () => {
      fireEvent.click(start());
    });
    expect(ipc.agentsTaskCreate).toHaveBeenCalledWith(
      expect.objectContaining({ provider: { provider: "openai", model: "gpt-6.2-sol", effort: "medium" } }),
    );
    // Sent: the composer is back on the default.
    expect(optionsText()).toBe("Options: claude-opus-5-5 · Medium · Ask · Ask before each change");
  });

  it("opens provider settings from the model menu through the shared command", () => {
    render(<TaskComposer onCreated={() => {}} />);
    openOptions("Model");
    (modelMenu.props!.onManageProviders as () => void)();
    expect(registryRun).toHaveBeenCalledWith("agents.providerSettings");
  });

  it("names the provider that needs a key", () => {
    useAgents.setState({
      status: { ...status, connections: [{ ...status.connections[0]!, configured: false }, status.connections[1]!], configured: false },
    });
    render(<TaskComposer onCreated={() => {}} />);
    expect(screen.getByText(/Add an Anthropic API key before starting a task/)).toBeTruthy();
  });
});

describe("follow-up", () => {
  beforeEach(() => {
    const one = detail(summary());
    const two = detail(summary({ id: "task-2", title: "Other task" }));
    ipc.agentsTaskDetail.mockImplementation((id: string) => Promise.resolve(id === "task-2" ? two : one));
    useAgents.setState({ tasks: [one.summary, two.summary], details: { "task-1": one, "task-2": two } });
  });

  it("keeps each task's draft to itself", async () => {
    const { rerender } = render(<TaskView id="task-1" />);
    fireEvent.change(screen.getByTestId("agents-follow-up-text"), { target: { value: "Only for task one" } });
    rerender(<TaskView id="task-2" />);
    expect((screen.getByTestId("agents-follow-up-text") as HTMLTextAreaElement).value).toBe("");
    expect(drafts.get(NEW_TASK_DRAFT).text).toBe("");
    rerender(<TaskView id="task-1" />);
    expect((screen.getByTestId("agents-follow-up-text") as HTMLTextAreaElement).value).toBe("Only for task one");
  });

  it("sends attachments and a model switch, and shows the backend's refusal while keeping the draft", async () => {
    ipc.agentsModelsList.mockResolvedValue({
      provider: "anthropic",
      state: "fresh",
      fetchedAtMs: Date.now(),
      error: null,
      models: [],
      suggested: null,
      suggestedReason: null,
      metadataChecked: "2026-11-17",
    } satisfies ModelCatalog);
    ipc.agentsTaskContinue.mockRejectedValueOnce(
      new Error("This conversation cannot continue on gpt-4.1-nano: OpenAI model gpt-4.1-nano is not known to accept PDF documents."),
    );
    render(<TaskView id="task-1" />);
    const field = screen.getByTestId("agents-follow-up-text");
    fireEvent.change(field, { target: { value: "Now the appendix" } });
    fireEvent.paste(field, { clipboardData: clipboard({ files: [png([5])] }) });
    await screen.findByTestId("agents-attachment");
    const attachment = screen.getByTestId("agents-attachment").dataset.attachmentId;
    openOptions("Model");
    // The menu knows the model the task runs on; nothing is chosen yet.
    expect(JSON.parse(screen.getByTestId("model-menu").dataset.running!)).toEqual({ provider: "anthropic", model: "claude-opus-5-5", effort: "medium" });
    fireEvent.click(screen.getByRole("button", { name: "Pick gpt-4.1-nano" }));
    await act(async () => {
      fireEvent.click(screen.getByTestId("agents-continue"));
    });
    expect(ipc.agentsTaskContinue).toHaveBeenCalledWith("task-1", "Now the appendix", null, [attachment], {
      provider: "openai",
      model: "gpt-4.1-nano",
      effort: "medium",
    });
    expect(screen.getByTestId("agents-follow-up-error").textContent).toContain("This conversation cannot continue on gpt-4.1-nano");
    expect((field as HTMLTextAreaElement).value).toBe("Now the appendix");
    expect(screen.getByTestId("agents-attachment").dataset.attachmentId).toBe(attachment);

    // Keeping the current model sends no switch.
    ipc.agentsTaskContinue.mockResolvedValueOnce(summary({ status: { kind: "queued" } }));
    openOptions("Model");
    fireEvent.click(screen.getByRole("button", { name: "Inherit" }));
    await act(async () => {
      fireEvent.click(screen.getByTestId("agents-continue"));
    });
    expect(ipc.agentsTaskContinue).toHaveBeenLastCalledWith("task-1", "Now the appendix", null, [attachment], null);
    expect(drafts.get("task-1").text).toBe("");
    expect(drafts.get("task-1").attachments).toEqual([]);
  });
});

describe("transcript attachments", () => {
  it("shows a message's attachments with thumbnails, and marks missing content", async () => {
    const shot = image("Pasted image 1.png");
    const gone = info("old-notes.txt", { missing: true });
    render(<TranscriptEntry item={{ kind: "userText", seq: 1, text: "Use the attached files.", attachments: [shot, gone] }} />);
    const list = screen.getByRole("list", { name: "Attachments" });
    const rows = within(list).getAllByTestId("agents-attachment");
    expect(rows.map((r) => r.dataset.attachmentId)).toEqual([shot.id, gone.id]);
    expect(rows[0]!.textContent).toContain("PNG image · 48.2 KiB · 1280×720");
    expect(rows[1]!.dataset.missing).toBe("true");
    expect(rows[1]!.textContent).toContain("Content no longer available");
    await waitFor(() => expect(within(rows[0]!).getByTestId("agents-attachment-thumb").getAttribute("src")).toMatch(/^data:image\/png;base64,/));
    expect(ipc.agentsAttachmentPreview).toHaveBeenCalledTimes(1);
    expect(ipc.agentsAttachmentPreview).toHaveBeenCalledWith(shot.id);
    expect(within(list).queryByRole("button")).toBeNull();
  });
});
