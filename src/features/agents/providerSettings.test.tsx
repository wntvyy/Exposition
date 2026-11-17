// Provider settings: each connection has its own key (OpenAI never touches the
// Anthropic one), OpenAI's optional organization/project context, each
// connection's state, the model picker for the default, and saving.
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ModelCatalog } from "../../bindings/ModelCatalog";
import type { ProviderStatus } from "../../bindings/ProviderStatus";

const ipc = vi.hoisted(() => ({
  agentsProviderSetKey: vi.fn(),
  agentsProviderClearKey: vi.fn(),
  agentsProviderUpdate: vi.fn(),
  agentsModelsList: vi.fn(),
}));
vi.mock("../../lib/ipc", () => ({
  commands: ipc,
  events: { agents: vi.fn(), agentsResync: vi.fn() },
  errorMessage: (e: unknown) => (e instanceof Error ? e.message : String(e)),
}));

const { ProviderSettingsDialog } = await import("./ProviderSettingsDialog");
const { initialAgentsState, useAgents } = await import("./store");

function status(openaiConfigured: boolean, overrides: Partial<ProviderStatus> = {}): ProviderStatus {
  return {
    settings: { provider: "anthropic", model: "claude-opus-5-5", effort: "high", consent: "scopeContent", openai: { organization: null, project: "proj_abc" } },
    configured: true,
    connections: [
      { provider: "anthropic", auth: "apiKey", configured: true, credentialError: null, context: null, version: 1 },
      { provider: "openai", auth: "apiKey", configured: openaiConfigured, credentialError: null, context: openaiConfigured ? "Project proj_abc" : null, version: 1 },
    ],
    credentialStore: "Windows Credential Manager",
    credentialError: null,
    scriptedAvailable: false,
    ...overrides,
  };
}

function catalog(provider: "anthropic" | "openai", ids: string[]): ModelCatalog {
  return {
    provider,
    state: "fresh",
    fetchedAtMs: Date.now(),
    error: null,
    models: ids.map((id, i) => ({
      provider,
      id,
      displayName: id,
      lifecycle: { kind: "active" },
      agent: { kind: "supported" },
      capabilities: { tools: "yes", images: "yes", pdf: "yes", efforts: ["low", "medium", "high"], contextTokens: null, maxOutputTokens: null },
      source: "catalog",
      createdAtMs: 10 - i,
    })),
    suggested: ids[0] ?? null,
    suggestedReason: ids[0] ? "The newest model Exposition supports for agents on this connection." : null,
    metadataChecked: "2026-11-17",
  };
}

beforeEach(() => {
  for (const fn of Object.values(ipc)) fn.mockReset();
  ipc.agentsModelsList.mockImplementation((provider: "anthropic" | "openai") =>
    Promise.resolve(provider === "openai" ? catalog("openai", ["gpt-6.2-sol", "gpt-6.2-luna"]) : catalog("anthropic", ["claude-opus-5-5", "claude-sonnet-5-5"])),
  );
  useAgents.setState({ ...initialAgentsState, settingsOpen: true, status: status(false) });
});
afterEach(cleanup);

const keyStatus = () => screen.getByTestId("agents-key-status");

describe("provider settings dialog", () => {
  it("saves an OpenAI key to its own connection without touching the Anthropic key", async () => {
    const key = "sk-proj-unit-test-not-a-real-key-31d9";
    ipc.agentsProviderSetKey.mockResolvedValue(status(true));
    render(<ProviderSettingsDialog />);
    expect(keyStatus().dataset.provider).toBe("anthropic");
    expect(keyStatus().textContent).toContain("Configured");
    fireEvent.click(screen.getByRole("radio", { name: "OpenAI" }));
    expect(keyStatus().dataset.provider).toBe("openai");
    expect(keyStatus().textContent).toContain("Not configured");
    // Only the shown connection's key field exists.
    expect(screen.getAllByLabelText(/API key/)).toHaveLength(1);
    expect((screen.getByLabelText("API key") as HTMLInputElement).placeholder).toBe("Paste your OpenAI API key");
    fireEvent.change(screen.getByLabelText("API key"), { target: { value: key } });
    await act(async () => {
      fireEvent.click(screen.getByTestId("agents-key-save"));
    });
    expect(ipc.agentsProviderSetKey).toHaveBeenCalledTimes(1);
    expect(ipc.agentsProviderSetKey).toHaveBeenCalledWith(key, "openai");
    expect(ipc.agentsProviderClearKey).not.toHaveBeenCalled();
    await screen.findByText("OpenAI API key saved to the credential store.");
    expect(keyStatus().textContent).toContain("Configured");
    expect(document.documentElement.outerHTML.includes(key)).toBe(false);
    // The Anthropic connection is unchanged.
    fireEvent.click(screen.getByRole("radio", { name: "Anthropic" }));
    expect(keyStatus().textContent).toContain("Configured");
  });

  it("saves the OpenAI organization and project with the settings, and shows the backend's refusal", async () => {
    ipc.agentsProviderUpdate.mockRejectedValueOnce(new Error("The OpenAI organization ID should look like org-…"));
    ipc.agentsProviderUpdate.mockResolvedValueOnce(status(false));
    render(<ProviderSettingsDialog />);
    fireEvent.click(screen.getByRole("radio", { name: "OpenAI" }));
    const context = screen.getByTestId("agents-openai-context");
    expect((within(context).getByLabelText("Project ID (optional)") as HTMLInputElement).value).toBe("proj_abc");
    expect(screen.getByText(/billed to its OpenAI API account/)).toBeTruthy();
    expect(screen.queryByText(/ChatGPT/)).toBeNull();
    fireEvent.change(within(context).getByLabelText("Organization ID (optional)"), { target: { value: "acme" } });
    await screen.findByRole("listbox", { name: "Models" });
    await act(async () => {
      fireEvent.click(screen.getByTestId("agents-settings-save"));
    });
    expect(ipc.agentsProviderUpdate).toHaveBeenCalledWith({
      provider: "anthropic",
      model: "claude-opus-5-5",
      effort: "high",
      consent: "scopeContent",
      openai: { organization: "acme", project: "proj_abc" },
    });
    expect(screen.getByTestId("agents-settings-error").textContent).toContain("should look like org-");
    fireEvent.change(within(context).getByLabelText("Organization ID (optional)"), { target: { value: "" } });
    await act(async () => {
      fireEvent.click(screen.getByTestId("agents-settings-save"));
    });
    expect(ipc.agentsProviderUpdate).toHaveBeenLastCalledWith(expect.objectContaining({ openai: { organization: null, project: "proj_abc" } }));
    await screen.findByText("Settings saved.");
  });

  it("chooses the default model from the picker (no typing) and saves the exact ID", async () => {
    ipc.agentsProviderUpdate.mockResolvedValue(status(true));
    useAgents.setState({ status: status(true) });
    render(<ProviderSettingsDialog />);
    await screen.findByRole("listbox", { name: "Models" });
    expect(screen.queryByRole("textbox", { name: "Model" })).toBeNull();
    // OpenAI: the picker lists that connection's models and takes its suggestion.
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: /Provider/ }));
    });
    const openai = await screen.findByRole("option", { name: "OpenAI" });
    expect(openai.textContent).toContain("API key configured");
    await act(async () => {
      fireEvent.click(openai);
    });
    await waitFor(() => expect(screen.getByRole("option", { name: "gpt-6.2-luna" })).toBeTruthy());
    fireEvent.click(screen.getByRole("option", { name: "gpt-6.2-luna" }));
    await act(async () => {
      fireEvent.click(screen.getByTestId("agents-settings-save"));
    });
    expect(ipc.agentsProviderUpdate).toHaveBeenCalledWith(expect.objectContaining({ provider: "openai", model: "gpt-6.2-luna", effort: "high" }));
  });

  it("offers to add the key when the chosen provider has none, and goes to that connection", async () => {
    render(<ProviderSettingsDialog />);
    await screen.findByRole("listbox", { name: "Models" });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: /Provider/ }));
    });
    const openai = await screen.findByRole("option", { name: "OpenAI" });
    expect(openai.textContent).toContain("No API key");
    await act(async () => {
      fireEvent.click(openai);
    });
    expect(screen.getByText("Connect OpenAI to list its models")).toBeTruthy();
    // Nothing can be listed without a key.
    expect(ipc.agentsModelsList).not.toHaveBeenCalledWith("openai", expect.anything());
    fireEvent.click(screen.getByRole("button", { name: "Add API key…" }));
    expect(keyStatus().dataset.provider).toBe("openai");
  });
});
