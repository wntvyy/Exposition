// Transcript entries: agent text renders through the shared Markdown renderer
// in a response surface; tool payloads stay plain monospace; Run in Research
// hands code to the user's session or the Research input; the transcript
// follows new output only while the reader is at the bottom.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useRef, type ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { SessionInfo } from "../../bindings/SessionInfo";
import type { TranscriptItem } from "../../bindings/TranscriptItem";

const ipc = vi.hoisted(() => ({ appOpenExternal: vi.fn() }));
const research = vi.hoisted(() => ({
  userSession: vi.fn(),
  runCode: vi.fn(),
  setState: vi.fn(),
  getState: vi.fn(),
}));
const shell = vi.hoisted(() => ({ active: vi.fn(), show: vi.fn(), runCommand: vi.fn(), notify: vi.fn() }));

vi.mock("../../lib/ipc", () => ({ commands: ipc, errorMessage: (e: unknown) => String(e) }));
vi.mock("../research/store", () => ({
  userSession: research.userSession,
  runCode: research.runCode,
  useResearch: { setState: research.setState, getState: research.getState },
}));
vi.mock("../../app/pages", () => ({ EDITOR_PAGE: "editor", pages: { active: shell.active, show: shell.show } }));
vi.mock("../../app/registry", () => ({ registry: { runCommand: shell.runCommand } }));
vi.mock("../../app/notifications", () => ({ notify: shell.notify }));
vi.mock("../../ui/components/MarkdownHighlight", () => ({ highlight: vi.fn(async () => null) }));

const { Draft, TranscriptEntry, runInResearch, useFollowBottom } = await import("./Transcript");

function session(language: SessionInfo["language"], state: SessionInfo["state"]["kind"] = "idle"): SessionInfo {
  return { id: "s1", owner: { kind: "user" }, language, state: { kind: state } } as SessionInfo;
}

beforeEach(() => {
  for (const fn of [...Object.values(research), ...Object.values(shell), ipc.appOpenExternal]) fn.mockReset();
  ipc.appOpenExternal.mockResolvedValue(undefined);
  research.runCode.mockResolvedValue(null);
  shell.active.mockReturnValue("editor");
  research.getState.mockReturnValue({ code: "" });
});
afterEach(() => cleanup());

describe("transcript entries", () => {
  it("renders agent text as Markdown in a response surface with Copy", () => {
    const item: TranscriptItem = { kind: "assistantText", seq: 1, text: "**Bold** and $x^2$.\n\n```python\nprint(1)\n```" };
    const { container } = render(<TranscriptEntry item={item} />);
    const msg = screen.getByTestId("agents-assistant-text");
    expect(msg.querySelector(".ui-response")).not.toBeNull();
    expect(msg.querySelector("strong")?.textContent).toBe("Bold");
    expect(msg.querySelector(".katex")).not.toBeNull();
    expect(screen.getByRole("button", { name: "Copy response" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Run in Research" })).toBeTruthy();
    expect(container.querySelector("[role=log]")).toBeNull(); // the log role belongs to the transcript (TaskView)
  });

  it("keeps tool payloads, results, user text, and reasoning as plain text", () => {
    const markdown = "**bold** <script>x</script> [l](javascript:alert(1)) $x$";
    const items: TranscriptItem[] = [
      { kind: "userText", seq: 1, text: markdown },
      { kind: "thinking", seq: 2, text: markdown },
      { kind: "toolCall", seq: 3, toolUseId: "t", name: "read_file", input: { path: "a.md", note: markdown }, status: "succeeded", result: markdown, isError: false },
    ];
    const { container } = render(
      <>
        {items.map((item) => (
          <TranscriptEntry key={item.seq} item={item} />
        ))}
      </>,
    );
    expect(container.querySelector("strong, .katex, [data-inert-link], .ui-response")).toBeNull();
    const pres = [...container.querySelectorAll("pre.agents-pre")].map((p) => p.textContent);
    expect(pres).toEqual([JSON.stringify({ path: "a.md", note: markdown }, null, 2), markdown]);
  });

  it("streams the draft without Copy and keeps its status line", () => {
    render(<Draft draft={{ turn: 1, text: "Partial **resp", thinking: "", activeTool: "read_file", activeToolStatus: "running" }} />);
    const draft = screen.getByTestId("agents-draft");
    expect(draft.textContent).toContain("Partial");
    expect(screen.queryByRole("button", { name: /Copy/ })).toBeNull();
    expect(draft.querySelector(".ui-status-text")).not.toBeNull();
  });

  it("opens links through app_open_external", () => {
    render(<TranscriptEntry item={{ kind: "assistantText", seq: 1, text: "[docs](https://example.org/docs)" }} />);
    fireEvent.click(screen.getByRole("link", { name: "docs" }));
    expect(ipc.appOpenExternal).toHaveBeenCalledWith("https://example.org/docs");
  });
});

describe("Run in Research", () => {
  it("runs in the user's session when one of that language is ready", async () => {
    research.userSession.mockReturnValue(session("python"));
    await runInResearch("print(1)", "python");
    expect(research.runCode).toHaveBeenCalledWith("print(1)");
    expect(research.setState).not.toHaveBeenCalled();
    expect(shell.runCommand).toHaveBeenCalledWith("research.show");
  });

  it("puts the code into the Research input and starts nothing when no session runs", async () => {
    research.userSession.mockReturnValue(undefined);
    await runInResearch("x <- 1", "r");
    expect(research.runCode).not.toHaveBeenCalled();
    expect(research.setState).toHaveBeenCalledWith({ code: "x <- 1" });
  });

  it("keeps code the user typed in the Research input and offers to replace it", async () => {
    research.userSession.mockReturnValue(undefined);
    research.getState.mockReturnValue({ code: "my_scratch = 42" });
    await runInResearch("print(1)", "python");
    expect(research.setState).not.toHaveBeenCalled();
    expect(shell.notify).toHaveBeenCalledTimes(1);
    const note = shell.notify.mock.calls[0]![0] as { actions: { label: string; run: () => void }[] };
    expect(note.actions[0]!.label).toBe("Replace input");
    note.actions[0]!.run();
    expect(research.setState).toHaveBeenCalledWith({ code: "print(1)" });
  });

  it("does not run code in a session of another language or one that is not ready", async () => {
    research.userSession.mockReturnValue(session("r"));
    await runInResearch("print(1)", "python");
    research.userSession.mockReturnValue(session("python", "starting"));
    await runInResearch("print(2)", "python");
    expect(research.runCode).not.toHaveBeenCalled();
    expect(research.setState).toHaveBeenLastCalledWith({ code: "print(2)" });
  });

  it("reveals the Research page when used outside the Editor page", async () => {
    shell.active.mockReturnValue("agents");
    research.userSession.mockReturnValue(undefined);
    await runInResearch("print(1)", "python");
    expect(shell.show).toHaveBeenCalledWith("research");
    expect(shell.runCommand).not.toHaveBeenCalled();
  });
});

describe("follow the bottom", () => {
  function Log({ children }: { children: ReactNode }) {
    const ref = useRef<HTMLDivElement>(null);
    useFollowBottom(ref, "task-1");
    return (
      <div ref={ref} data-testid="log" role="log">
        {children}
      </div>
    );
  }

  it("follows new output at the bottom and leaves a reader who scrolled up in place", async () => {
    let height = 1000;
    const scrollHeight = vi.spyOn(HTMLElement.prototype, "scrollHeight", "get").mockImplementation(() => height);
    const clientHeight = vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(200);
    try {
      const { rerender } = render(<Log>one</Log>);
      const log = screen.getByTestId("log");
      expect(log.scrollTop).toBe(1000);
      // New output while at the bottom: followed.
      height = 1400;
      rerender(<Log>one two</Log>);
      await act(async () => {});
      expect(log.scrollTop).toBe(1400);
      // The reader scrolls up: new output does not move them.
      log.scrollTop = 100;
      fireEvent.scroll(log);
      height = 1800;
      rerender(<Log>one two three</Log>);
      await act(async () => {});
      expect(log.scrollTop).toBe(100);
      // Back at the end: following resumes.
      log.scrollTop = 1800 - 200;
      fireEvent.scroll(log);
      height = 2200;
      rerender(<Log>one two three four</Log>);
      await act(async () => {});
      expect(log.scrollTop).toBe(2200);
    } finally {
      scrollHeight.mockRestore();
      clientHeight.mockRestore();
    }
  });
});
