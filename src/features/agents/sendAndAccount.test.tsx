// Regressions from the independent review of the agents work: attachments
// stay put while their send runs, a finished send never cuts a word the user
// typed meanwhile (the picker refetch on a key change is in modelMenu.test.tsx).
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { AttachmentInfo } from "../../bindings/AttachmentInfo";
import type { ProviderStatus } from "../../bindings/ProviderStatus";

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

const { TaskComposer } = await import("./TaskComposer");
const { drafts, NEW_TASK_DRAFT, remainingText } = await import("./drafts");
const { initialAgentsState, useAgents } = await import("./store");

const status = (openaiVersion = 1): ProviderStatus => ({
  settings: { provider: "openai", model: "gpt-6.2-sol", effort: "medium", consent: "scopeContent" },
  configured: true,
  connections: [
    { provider: "anthropic", auth: "apiKey", configured: true, credentialError: null, context: null, version: 1 },
    { provider: "openai", auth: "apiKey", configured: true, credentialError: null, context: null, version: openaiVersion },
  ],
  credentialStore: "test store",
  credentialError: null,
  scriptedAvailable: false,
});

const att: AttachmentInfo = {
  id: "att-1",
  name: "notes.md",
  mediaType: "text/markdown",
  kind: "text",
  sizeBytes: 10,
  width: null,
  height: null,
  sha256: "0".repeat(64),
  missing: false,
};


beforeEach(() => {
  for (const fn of Object.values(ipc)) fn.mockReset();
  ipc.agentsAttachmentsDiscard.mockResolvedValue(undefined);
  ipc.agentsAttachmentPreview.mockResolvedValue(null);
  drafts.resetForTests();
  useAgents.setState({ ...initialAgentsState, status: status(), workspace: { root: "C:/paper", name: "paper" } as never });
});
afterEach(cleanup);

describe("sending", () => {
  it("does not offer Remove for an attachment while its send is running", async () => {
    ipc.agentsAttachmentsPick.mockResolvedValue([{ kind: "added", attachment: att }]);
    let resolveCreate!: (v: unknown) => void;
    ipc.agentsTaskCreate.mockReturnValue(new Promise((r) => (resolveCreate = r)));
    render(<TaskComposer onCreated={() => {}} />);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Attach files…" }));
    });
    expect(screen.getByRole("button", { name: "Remove notes.md" })).toBeTruthy();
    await act(async () => {
      fireEvent.click(screen.getByTestId("agents-start"));
    });
    expect(ipc.agentsTaskCreate.mock.calls[0]![0].attachments).toEqual(["att-1"]);
    expect(screen.queryByRole("button", { name: "Remove notes.md" })).toBeNull();
    expect(ipc.agentsAttachmentsDiscard).not.toHaveBeenCalled();
    await act(async () => resolveCreate({ id: "task-1" }));
  });

  it("never cuts a word typed onto the sent text", () => {
    expect(remainingText("Fix", "Fix")).toBe("");
    expect(remainingText("Fixed the intro", "Fix")).toBe("Fixed the intro");
    expect(remainingText("Fix\nAlso the outro", "Fix")).toBe("Also the outro");
    expect(remainingText("Fix the intro", "Fix")).toBe("the intro");
    drafts.setText(NEW_TASK_DRAFT, "Fixed the intro");
    drafts.completeSend(NEW_TASK_DRAFT, { text: "Fix", attachments: [], model: null });
    expect(drafts.get(NEW_TASK_DRAFT).text).toBe("Fixed the intro");
  });
});
