// Every "Provider settings" entry point (the palette command, the composer's
// and the model menu's links through `agents.openSettings`) opens Settings at
// Model providers, from the Agent view and the Agents page alike, and the
// composer's draft survives the trip.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const editorsMock = vi.hoisted(() => ({ open: vi.fn() }));
vi.mock("../../app/editors", async (original) => {
  const actual = await original<typeof import("../../app/editors")>();
  return { ...actual, editors: { ...actual.editors, open: editorsMock.open } };
});
vi.mock("../../lib/ipc", () => ({
  commands: {},
  events: { agents: vi.fn(), agentsResync: vi.fn(), workspace: vi.fn(), documents: vi.fn() },
  errorMessage: (e: unknown) => String(e),
}));
vi.mock("./DiffView", () => ({ DiffView: () => null }));

const { registry } = await import("../../app/registry");
await import("./register");
const { agents } = await import("./store");
const { drafts, NEW_TASK_DRAFT } = await import("./drafts");
const { useSettings, initialSettingsState } = await import("../settings/store");
const { SETTINGS_RESOURCE } = await import("../settings/model");

beforeEach(() => {
  editorsMock.open.mockReset();
  useSettings.setState({ ...initialSettingsState });
});
afterEach(() => vi.clearAllMocks());

describe("provider settings entry points", () => {
  it("the palette command opens Settings at Model providers", async () => {
    const command = registry.command("agents.providerSettings");
    expect(command?.title).toBe("Agent Provider Settings");
    await registry.runCommand("agents.providerSettings");
    expect(useSettings.getState().section).toBe("providers");
    expect(editorsMock.open).toHaveBeenCalledWith({ ...SETTINGS_RESOURCE });
  });

  it("the composer and model-menu links open the same section and keep the draft", async () => {
    drafts.setText(NEW_TASK_DRAFT, "Tighten the proof of Lemma 2");
    drafts.setModel(NEW_TASK_DRAFT, { provider: "openai", model: "gpt-6.2-sol", effort: "high" });
    const before = drafts.get(NEW_TASK_DRAFT);
    agents.openSettings(true);
    await vi.waitFor(() => expect(editorsMock.open).toHaveBeenCalledWith({ ...SETTINGS_RESOURCE }));
    expect(useSettings.getState().section).toBe("providers");
    expect(drafts.get(NEW_TASK_DRAFT)).toBe(before);
  });
});
