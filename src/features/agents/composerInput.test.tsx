// Composer input: Enter sends through Send (once, same rules), Shift+Enter is
// a new line, an IME's confirming Enter never sends, a held key and a second
// Enter send nothing more; a large text paste becomes a plain-text attachment
// (thresholds, exact UTF-8 bytes, numbering, selection replaced only after
// the attachment exists, Undo / Insert as text, failures keep the draft,
// mixed clipboards, restoration of the draft).
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
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
  agentsTaskDetail: vi.fn(),
  agentsModelsList: vi.fn(),
}));
vi.mock("../../lib/ipc", () => ({
  commands: ipc,
  events: { agents: vi.fn(), agentsResync: vi.fn() },
  errorMessage: (e: unknown) => (e instanceof Error ? e.message : String(e)),
}));
vi.mock("./ModelMenu", () => ({ ModelMenu: () => <div data-testid="model-menu" /> }));

const { Composer, TaskComposer, DEFAULT_BUDGETS, KEY_HINT, resetComposerForTests } = await import("./TaskComposer");
const { drafts, NEW_TASK_DRAFT, isLargePaste, lineCount, utf8Base64, clipboardHasFiles, LARGE_PASTE_CHARS, LARGE_PASTE_LINES, MAX_ATTACHMENTS_PER_MESSAGE, MAX_REQUEST_CHARS } =
  await import("./drafts");
const { initialAgentsState, useAgents } = await import("./store");

let nextId = 0;
function info(name: string, overrides: Partial<AttachmentInfo> = {}): AttachmentInfo {
  const n = ++nextId;
  return {
    id: `att-${n}`,
    name,
    mediaType: "text/plain",
    kind: "text",
    sizeBytes: 9000,
    width: null,
    height: null,
    sha256: n.toString(16).padStart(64, "0"),
    missing: false,
    ...overrides,
  };
}
const added = (attachment: AttachmentInfo): IngestOutcome => ({ kind: "added", attachment });

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

/** A paste event's clipboard: string items and file items (as WebView2 offers them). */
function clipboard({ text, files = [] }: { text?: string; files?: File[] }) {
  const items = [
    ...(text !== undefined ? [{ kind: "string", type: "text/plain", getAsFile: () => null }] : []),
    ...files.map((file) => ({ kind: "file", type: file.type, getAsFile: () => file })),
  ];
  return { items, files, types: items.map((i) => i.type), getData: (type: string) => (type === "text/plain" ? (text ?? "") : "") };
}

const field = () => screen.getByTestId("agents-prompt") as HTMLTextAreaElement;
const type = (text: string) => fireEvent.change(field(), { target: { value: text } });
/** Press a key in the field; false when the composer prevented the default (no line break is inserted). */
const press = (init: KeyboardEventInit & { keyCode?: number }, target: Element = field()) => fireEvent.keyDown(target, { key: "Enter", ...init });
const chips = () => screen.queryAllByTestId("agents-attachment").map((r) => r.querySelector(".ui-chip__label")?.textContent ?? "");
const decoded = (base64: string) => new TextDecoder("utf-8", { fatal: true }).decode(Uint8Array.from(atob(base64), (c) => c.charCodeAt(0)));
/** Paste text into the field; false when the composer took the paste (nothing is inserted). */
const paste = (text: string, files: File[] = []) => fireEvent.paste(field(), { clipboardData: clipboard({ text, files }) });
const large = (seed = "x") => seed.repeat(LARGE_PASTE_CHARS + 1);

beforeEach(() => {
  for (const fn of Object.values(ipc)) fn.mockReset();
  ipc.agentsAttachmentPreview.mockResolvedValue(null);
  ipc.agentsAttachmentsDiscard.mockResolvedValue(undefined);
  ipc.agentsAttachmentsAddBytes.mockImplementation((name: string, mediaType: string) =>
    Promise.resolve(added(mediaType === "text/plain" ? info(name) : info(name, { kind: "image", mediaType, width: 4, height: 3 }))),
  );
  ipc.agentsTaskCreate.mockResolvedValue(summary({ id: "task-9" }));
  ipc.agentsTaskContinue.mockResolvedValue(summary());
  drafts.resetForTests();
  resetComposerForTests();
  useAgents.setState({ ...initialAgentsState, status, workspace: { root: "C:/paper", name: "paper" } as never });
});
afterEach(cleanup);

describe("composer keys", () => {
  it("Enter sends once through Send; Shift+Enter, an IME's Enter, and a held key send nothing", async () => {
    const created = deferred<TaskSummary>();
    ipc.agentsTaskCreate.mockReturnValue(created.promise);
    const onCreated = vi.fn();
    render(<TaskComposer onCreated={onCreated} />);
    type("Résumé の要約");
    expect(field().getAttribute("aria-describedby")).toBeTruthy();
    expect(document.getElementById(field().getAttribute("aria-describedby")!)?.textContent).toBe(KEY_HINT);

    // Shift+Enter is left to the field (it inserts the line break itself).
    expect(press({ shiftKey: true })).toBe(true);
    // The Enter that confirms a composition is left to the input method.
    expect(press({ isComposing: true })).toBe(true);
    expect(press({ keyCode: 229 })).toBe(true);
    expect(ipc.agentsTaskCreate).not.toHaveBeenCalled();
    // A held Enter (auto-repeat) neither sends nor breaks the line.
    expect(press({ repeat: true })).toBe(false);
    expect(ipc.agentsTaskCreate).not.toHaveBeenCalled();

    // Enter: no line break, one request; more Enters while it runs add none.
    act(() => {
      expect(press({})).toBe(false);
      expect(press({})).toBe(false);
    });
    act(() => {
      press({});
      press({ ctrlKey: true });
      fireEvent.click(screen.getByTestId("agents-start"));
    });
    expect(ipc.agentsTaskCreate).toHaveBeenCalledTimes(1);
    expect(ipc.agentsTaskCreate).toHaveBeenCalledWith(expect.objectContaining({ prompt: "Résumé の要約" }));
    await act(async () => created.resolve(summary({ id: "task-9" })));
    expect(onCreated).toHaveBeenCalledTimes(1);
    expect(field().value).toBe("");
  });

  it("Ctrl+Enter still sends, and an Enter outside the request field is not the composer's", async () => {
    render(<TaskComposer onCreated={() => {}} />);
    type("Fit the model");
    press({}, screen.getByTestId("agents-options"));
    expect(ipc.agentsTaskCreate).not.toHaveBeenCalled();
    await act(async () => {
      press({ ctrlKey: true });
    });
    expect(ipc.agentsTaskCreate).toHaveBeenCalledTimes(1);
  });

  it("does nothing when Send is unavailable, and keeps the draft", async () => {
    const pick = deferred<IngestOutcome[]>();
    ipc.agentsAttachmentsPick.mockReturnValue(pick.promise);
    render(<TaskComposer onCreated={() => {}} />);
    // Empty draft.
    expect(press({})).toBe(false);
    // Whitespace only.
    type("  \n ");
    press({});
    expect(field().value).toBe("  \n ");
    // An attachment is still being added.
    type("Summarize the data");
    act(() => {
      fireEvent.click(screen.getByRole("button", { name: "Attach files…" }));
    });
    expect((screen.getByTestId("agents-start") as HTMLButtonElement).disabled).toBe(true);
    press({});
    expect(ipc.agentsTaskCreate).not.toHaveBeenCalled();
    expect(field().value).toBe("Summarize the data");
    // The provider is not configured.
    await act(async () => pick.resolve([]));
    act(() => useAgents.setState({ status: { ...status, connections: status.connections.map((c) => ({ ...c, configured: false })) } }));
    press({});
    expect(ipc.agentsTaskCreate).not.toHaveBeenCalled();
    expect(field().value).toBe("Summarize the data");
    // Available again: the same Enter sends the kept draft.
    act(() => useAgents.setState({ status }));
    await act(async () => {
      press({});
    });
    expect(ipc.agentsTaskCreate).toHaveBeenCalledWith(expect.objectContaining({ prompt: "Summarize the data" }));
  });

  it("an attachment-only message is sent by Enter as by Send", async () => {
    const doc = info("data.csv", { mediaType: "text/csv" });
    ipc.agentsAttachmentsPick.mockResolvedValue([added(doc)]);
    render(<TaskComposer onCreated={() => {}} />);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Attach files…" }));
    });
    await act(async () => {
      press({});
    });
    expect(ipc.agentsTaskCreate).toHaveBeenCalledWith(expect.objectContaining({ prompt: "", attachments: [doc.id] }));
  });

  it("a follow-up is sent by Enter once; while the task works or a question waits nothing is sent or queued", async () => {
    const view = render(<Composer task={summary()} />);
    const followUp = () => screen.getByTestId("agents-follow-up-text") as HTMLTextAreaElement;
    fireEvent.change(followUp(), { target: { value: "Now the table" } });
    await act(async () => {
      press({}, followUp());
      press({}, followUp());
    });
    expect(ipc.agentsTaskContinue).toHaveBeenCalledTimes(1);
    expect(ipc.agentsTaskContinue).toHaveBeenCalledWith("task-1", "Now the table", null, [], null);
    expect(followUp().value).toBe("");

    ipc.agentsTaskContinue.mockClear();
    for (const kind of ["running", "awaitingAnswer"] as const) {
      view.rerender(<Composer task={summary({ status: { kind } as TaskSummary["status"] })} />);
      fireEvent.change(followUp(), { target: { value: "And the caption" } });
      expect(press({}, followUp())).toBe(false);
      expect(ipc.agentsTaskContinue).not.toHaveBeenCalled();
      expect(followUp().value).toBe("And the caption");
      expect(press({ shiftKey: true }, followUp())).toBe(true);
    }
    expect(ipc.agentsTaskCreate).not.toHaveBeenCalled();
  });
});

describe("large paste threshold", () => {
  it("is more than 8,000 characters or more than 100 lines", () => {
    expect([LARGE_PASTE_CHARS, LARGE_PASTE_LINES]).toEqual([8000, 100]);
    expect(isLargePaste("")).toBe(false);
    expect(isLargePaste("x".repeat(8000))).toBe(false);
    expect(isLargePaste("x".repeat(8001))).toBe(true);
    const lines = (n: number) => Array.from({ length: n }, (_, i) => `line ${i + 1}`).join("\n");
    expect(lineCount(lines(100))).toBe(100);
    expect(isLargePaste(lines(100))).toBe(false);
    // A final line break does not start a 101st line.
    expect(isLargePaste(`${lines(100)}\n`)).toBe(false);
    expect(isLargePaste(lines(101))).toBe(true);
    // Windows and old Mac line breaks count once each.
    expect(lineCount("a\r\nb\rc\n")).toBe(3);
    expect(isLargePaste(Array.from({ length: 101 }, () => "x").join("\r\n"))).toBe(true);
  });

  it("encodes the exact text as UTF-8", () => {
    const text = "α → β\r\n\t数据 🙂  \n";
    expect(decoded(utf8Base64(text))).toBe(text);
    expect(decoded(utf8Base64("é".repeat(70_000)))).toBe("é".repeat(70_000));
  });

  it("tells files on the clipboard from text", () => {
    expect(clipboardHasFiles(clipboard({ text: "x" }) as never)).toBe(false);
    expect(clipboardHasFiles(clipboard({ text: "x", files: [new File(["a"], "a.txt", { type: "text/plain" })] }) as never)).toBe(true);
    expect(clipboardHasFiles(null)).toBe(false);
  });
});

describe("large text paste", () => {
  it("leaves a paste at the threshold to the field and attaches one above it, exactly, without sending", async () => {
    render(<TaskComposer onCreated={() => {}} />);
    type("Review this:");
    expect(paste("x".repeat(LARGE_PASTE_CHARS))).toBe(true);
    expect(paste(Array.from({ length: LARGE_PASTE_LINES }, () => "y").join("\n"))).toBe(true);
    expect(ipc.agentsAttachmentsAddBytes).not.toHaveBeenCalled();

    // One very long line.
    const line = large("z");
    expect(paste(line)).toBe(false);
    await waitFor(() => expect(chips()).toEqual(["pasted-text-1.txt"]));
    const [name, mediaType, data] = ipc.agentsAttachmentsAddBytes.mock.calls[0]!;
    expect([name, mediaType]).toEqual(["pasted-text-1.txt", "text/plain"]);
    expect(decoded(data)).toBe(line);
    // The request text is untouched and nothing was sent.
    expect(field().value).toBe("Review this:");
    expect(ipc.agentsTaskCreate).not.toHaveBeenCalled();
    // The chip is an ordinary text attachment: size and a removal control.
    const chip = screen.getByTestId("agents-attachment");
    expect(chip.dataset.kind).toBe("text");
    expect(chip.textContent).toContain("Text · 8.8 KiB");
    expect(screen.getByRole("button", { name: "Remove pasted-text-1.txt" })).toBeTruthy();
    expect(screen.getByTestId("agents-paste-notice").textContent).toContain("The pasted text was attached as pasted-text-1.txt.");

    // Multiline code with Windows line breaks, tabs, and non-ASCII text: the same bytes.
    const code = Array.from({ length: 120 }, (_, i) => `\tlet x${i} = "héllo — 数据 🙂";  `).join("\r\n");
    expect(paste(code)).toBe(false);
    await waitFor(() => expect(chips()).toEqual(["pasted-text-1.txt", "pasted-text-2.txt"]));
    expect(decoded(ipc.agentsAttachmentsAddBytes.mock.calls[1]![2])).toBe(code);

    // Sent as attachments of the message, like any file.
    await act(async () => {
      fireEvent.click(screen.getByTestId("agents-start"));
    });
    const request = ipc.agentsTaskCreate.mock.calls[0]![0];
    expect(request.prompt).toBe("Review this:");
    expect(request.attachments).toHaveLength(2);
    expect(screen.queryByTestId("agents-paste-notice")).toBeNull();
    expect(drafts.get(NEW_TASK_DRAFT).pastes).toEqual([]);
  });

  it("replaces the selection only once the attachment exists; Undo puts it back", async () => {
    const ingest = deferred<IngestOutcome>();
    ipc.agentsAttachmentsAddBytes.mockReturnValue(ingest.promise);
    render(<TaskComposer onCreated={() => {}} />);
    type("Compare OLD with this");
    field().setSelectionRange(8, 11);
    expect(paste(large())).toBe(false);
    // Still being attached: the selected text is not consumed, and Send waits.
    expect(field().value).toBe("Compare OLD with this");
    expect(screen.getByText("Adding attachments…")).toBeTruthy();
    expect((screen.getByTestId("agents-start") as HTMLButtonElement).disabled).toBe(true);
    const attachment = info("pasted-text-1.txt");
    await act(async () => ingest.resolve(added(attachment)));
    expect(field().value).toBe("Compare  with this");
    expect(chips()).toEqual(["pasted-text-1.txt"]);

    fireEvent.click(screen.getByTestId("agents-paste-undo"));
    expect(field().value).toBe("Compare OLD with this");
    expect(chips()).toEqual([]);
    expect(screen.queryByTestId("agents-paste-notice")).toBeNull();
    await waitFor(() => expect(ipc.agentsAttachmentsDiscard).toHaveBeenCalledWith([attachment.id]));
  });

  it("keeps text typed while the paste was read, and replaces nothing then", async () => {
    const ingest = deferred<IngestOutcome>();
    ipc.agentsAttachmentsAddBytes.mockReturnValue(ingest.promise);
    render(<TaskComposer onCreated={() => {}} />);
    type("Compare OLD with this");
    field().setSelectionRange(8, 11);
    paste(large());
    type("Compare OLD with this, carefully");
    await act(async () => ingest.resolve(added(info("pasted-text-1.txt"))));
    expect(field().value).toBe("Compare OLD with this, carefully");
    fireEvent.click(screen.getByTestId("agents-paste-undo"));
    expect(field().value).toBe("Compare OLD with this, carefully");
    expect(chips()).toEqual([]);
  });

  it("Insert as text puts the text where it was pasted and removes the attachment", async () => {
    render(<TaskComposer onCreated={() => {}} />);
    type("Before  after");
    field().setSelectionRange(7, 7);
    const text = large("q");
    paste(text);
    await screen.findByTestId("agents-paste-notice");
    fireEvent.click(screen.getByTestId("agents-paste-inline"));
    expect(field().value).toBe(`Before ${text} after`);
    expect(chips()).toEqual([]);
    await waitFor(() => expect(ipc.agentsAttachmentsDiscard).toHaveBeenCalledTimes(1));

    // After the request text changed before that place, the text goes to the end.
    type("Other words");
    field().setSelectionRange(11, 11);
    paste(text);
    await screen.findByTestId("agents-paste-notice");
    type("Entirely different");
    fireEvent.click(screen.getByTestId("agents-paste-inline"));
    expect(field().value).toBe(`Entirely different${text}`);
  });

  it("offers no inline insertion beyond the request's length limit; the attachment stays", async () => {
    render(<TaskComposer onCreated={() => {}} />);
    paste("w".repeat(MAX_REQUEST_CHARS + 1));
    const notice = await screen.findByTestId("agents-paste-notice");
    expect(notice.textContent).toContain("too long to insert as text");
    expect(screen.queryByTestId("agents-paste-inline")).toBeNull();
    const [id] = drafts.get(NEW_TASK_DRAFT).attachments.map((a) => a.id);
    act(() => {
      expect(drafts.insertPasteInline(NEW_TASK_DRAFT, id!)).toBe(false);
    });
    expect(chips()).toEqual(["pasted-text-1.txt"]);
    expect(field().value).toBe("");
    expect(screen.getByTestId("agents-attachment-rejections").textContent).toContain("at most 100,000 characters");
  });

  it("numbers repeated pastes, and removing a chip removes its notice", async () => {
    render(<TaskComposer onCreated={() => {}} />);
    paste(large("a"));
    await waitFor(() => expect(chips()).toEqual(["pasted-text-1.txt"]));
    paste(large("a"));
    await waitFor(() => expect(chips()).toEqual(["pasted-text-1.txt", "pasted-text-2.txt"]));
    expect(screen.getByTestId("agents-paste-notice").textContent).toContain("pasted-text-2.txt");
    fireEvent.click(screen.getByRole("button", { name: "Remove pasted-text-2.txt" }));
    expect(screen.getByTestId("agents-paste-notice").textContent).toContain("pasted-text-1.txt");
    fireEvent.click(screen.getByRole("button", { name: "Remove pasted-text-1.txt" }));
    expect(screen.queryByTestId("agents-paste-notice")).toBeNull();
    paste(large("b"));
    await waitFor(() => expect(chips()).toEqual(["pasted-text-3.txt"]));
  });

  it("a failed attachment keeps the draft and the selection, says why, and frees the name", async () => {
    render(<TaskComposer onCreated={() => {}} />);
    type("Compare OLD with this");
    field().setSelectionRange(8, 11);
    ipc.agentsAttachmentsAddBytes.mockResolvedValueOnce({
      kind: "rejected",
      name: "pasted-text-1.txt",
      code: "attachments.textTooLarge",
      message: "The text file is 1.2 MiB; text files can be at most 1.0 MiB.",
    });
    expect(paste(large())).toBe(false);
    const rejections = await screen.findByTestId("agents-attachment-rejections");
    expect(rejections.textContent).toContain("pasted-text-1.txt: The text file is 1.2 MiB; text files can be at most 1.0 MiB.");
    expect(rejections.textContent).toContain("Nothing was pasted and the request is unchanged; the text is still on the clipboard.");
    expect(field().value).toBe("Compare OLD with this");
    expect([field().selectionStart, field().selectionEnd]).toEqual([8, 11]);
    expect(chips()).toEqual([]);
    expect(screen.queryByTestId("agents-paste-notice")).toBeNull();

    // The call itself fails.
    ipc.agentsAttachmentsAddBytes.mockRejectedValueOnce(new Error("The attachment could not be stored: disk full"));
    paste(large());
    await waitFor(() => expect(screen.getAllByTestId("agents-attachment-rejection")).toHaveLength(2));
    expect(field().value).toBe("Compare OLD with this");
    // The next paste that works is the first one.
    paste(large());
    await waitFor(() => expect(chips()).toEqual(["pasted-text-1.txt"]));
    expect(ipc.agentsTaskCreate).not.toHaveBeenCalled();
  });

  it("refuses a paste when the message already carries the most attachments, before reading anything", async () => {
    ipc.agentsAttachmentsPick.mockResolvedValue(Array.from({ length: MAX_ATTACHMENTS_PER_MESSAGE }, (_, i) => added(info(`file-${i}.txt`))));
    render(<TaskComposer onCreated={() => {}} />);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Attach files…" }));
    });
    type("Keep me");
    expect(paste(large())).toBe(false);
    expect(ipc.agentsAttachmentsAddBytes).not.toHaveBeenCalled();
    expect(screen.getByTestId("agents-attachment-rejections").textContent).toContain("A message can carry at most 20 attachments. Remove one, then paste again.");
    expect(chips()).toHaveLength(MAX_ATTACHMENTS_PER_MESSAGE);
    expect(field().value).toBe("Keep me");
  });

  it("the picker keeps only as many files as a message can carry and says so for the rest", async () => {
    const picked = Array.from({ length: MAX_ATTACHMENTS_PER_MESSAGE + 2 }, (_, i) => info(`file-${i}.txt`));
    ipc.agentsAttachmentsPick.mockResolvedValue(picked.map(added));
    render(<TaskComposer onCreated={() => {}} />);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Attach files…" }));
    });
    expect(chips()).toHaveLength(MAX_ATTACHMENTS_PER_MESSAGE);
    const lines = screen.getAllByTestId("agents-attachment-rejection").map((l) => l.textContent);
    expect(lines).toEqual([
      "file-20.txt: A message can carry at most 20 attachments. Remove one to attach this file.",
      "file-21.txt: A message can carry at most 20 attachments. Remove one to attach this file.",
    ]);
    await waitFor(() => expect(ipc.agentsAttachmentsDiscard).toHaveBeenCalledWith([picked[20]!.id, picked[21]!.id]));
  });

  it("with files or images on the clipboard, text is never also attached", async () => {
    render(<TaskComposer onCreated={() => {}} />);
    // An image with a large text representation (cells copied from a spreadsheet): the image, as before.
    const shot = new File([new Uint8Array([1, 2, 3])], "image.png", { type: "image/png" });
    expect(paste(large(), [shot])).toBe(true);
    await waitFor(() => expect(chips()).toEqual(["Pasted image 1.png"]));
    expect(ipc.agentsAttachmentsAddBytes).toHaveBeenCalledTimes(1);
    expect(ipc.agentsAttachmentsAddBytes.mock.calls[0]![1]).toBe("image/png");
    // A copied file that is not an image: nothing is attached from its text.
    const copied = new File(["a,b\n"], "data.csv", { type: "text/csv" });
    expect(paste(large(), [copied])).toBe(true);
    await new Promise((r) => setTimeout(r, 10));
    expect(ipc.agentsAttachmentsAddBytes).toHaveBeenCalledTimes(1);
    expect(chips()).toEqual(["Pasted image 1.png"]);
    expect(screen.queryByTestId("agents-paste-notice")).toBeNull();
  });

  it("stays with its conversation: restored when the composer is shown again, never in another draft", async () => {
    const ingest = deferred<IngestOutcome>();
    ipc.agentsAttachmentsAddBytes.mockReturnValue(ingest.promise);
    const view = render(<TaskComposer onCreated={() => {}} />);
    type("Draft for the new task");
    paste(large());
    // Navigate to a task before the paste was read.
    view.rerender(<Composer key="task-1" task={summary()} />);
    await act(async () => ingest.resolve(added(info("pasted-text-1.txt"))));
    expect(chips()).toEqual([]);
    expect(screen.queryByTestId("agents-paste-notice")).toBeNull();
    expect(drafts.get("task-1").attachments).toEqual([]);
    // Back at the new task: the text, the attachment, and the way back are there.
    view.rerender(<TaskComposer key="new" onCreated={() => {}} />);
    expect(field().value).toBe("Draft for the new task");
    expect(chips()).toEqual(["pasted-text-1.txt"]);
    expect(within(screen.getByTestId("agents-paste-notice")).getByTestId("agents-paste-undo")).toBeTruthy();
  });
});
