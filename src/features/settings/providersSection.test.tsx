// Settings › Model providers: each connection's state and key (save, replace,
// remove with confirmation; the key never stays in the DOM), credential-store
// errors, OpenAI's organization/project, the model list and its refresh, the
// defaults for new tasks through the model menu, propagation of provider
// events, and the moved project permissions. No ChatGPT sign-in is offered.
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ModelCatalog } from "../../bindings/ModelCatalog";
import type { ProviderKind } from "../../bindings/ProviderKind";
import type { ProviderStatus } from "../../bindings/ProviderStatus";

const ipc = vi.hoisted(() => ({
  agentsProviderStatus: vi.fn(() => new Promise(() => {})),
  agentsProviderSetKey: vi.fn(),
  agentsProviderClearKey: vi.fn(),
  agentsProviderUpdate: vi.fn(),
  agentsModelsList: vi.fn(),
}));
const core = vi.hoisted(() => ({ invoke: vi.fn((_command: string, _args?: unknown) => Promise.resolve([] as unknown)) }));
vi.mock("@tauri-apps/api/core", () => core);
vi.mock("../../lib/ipc", () => ({
  commands: ipc,
  events: {
    agents: vi.fn(() => new Promise(() => {})),
    agentsResync: vi.fn(),
    workspace: vi.fn(),
    documents: vi.fn(),
  },
  errorMessage: (e: unknown) => (e instanceof Error ? e.message : String(e)),
}));

const { ProvidersSection } = await import("./ProvidersSection");
const { initialAgentsState, reduceEvent, useAgents } = await import("../agents/store");
const { ConfirmHost } = await import("../../ui/components");

function status(openaiConfigured: boolean, overrides: Partial<ProviderStatus> = {}, openaiVersion = 1): ProviderStatus {
  return {
    settings: { provider: "anthropic", model: "claude-opus-5-5", effort: "high", consent: "scopeContent", openai: { organization: null, project: "proj_abc" } },
    configured: true,
    connections: [
      { provider: "anthropic", auth: "apiKey", configured: true, credentialError: null, context: null, version: 1 },
      {
        provider: "openai",
        auth: "apiKey",
        configured: openaiConfigured,
        credentialError: null,
        context: openaiConfigured ? "Project proj_abc" : null,
        version: openaiVersion,
      },
    ],
    credentialStore: "Windows Credential Manager",
    credentialError: null,
    scriptedAvailable: false,
    ...overrides,
  };
}

function catalog(provider: ProviderKind, ids: [string, string][], overrides: Partial<ModelCatalog> = {}): ModelCatalog {
  return {
    provider,
    state: "fresh",
    fetchedAtMs: Date.now(),
    error: null,
    models: ids.map(([id, displayName], i) => ({
      provider,
      id,
      displayName,
      lifecycle: { kind: "active" },
      agent: { kind: "supported" },
      capabilities: { tools: "yes", images: "yes", pdf: "yes", efforts: ["low", "medium", "high"], contextTokens: null, maxOutputTokens: null },
      source: "catalog",
      createdAtMs: 10 - i,
    })),
    suggested: null,
    suggestedReason: null,
    metadataChecked: "2026-11-17",
    ...overrides,
  };
}

const KEY = "sk-unit-test-not-a-real-key-7c1d";
const section = (provider: ProviderKind) => screen.getByTestId(`settings-provider-${provider}`);
const keyField = (provider: ProviderKind) => within(section(provider)).getByLabelText(/API key/) as HTMLInputElement;

function renderSection() {
  return render(
    <>
      <ProvidersSection />
      <ConfirmHost />
    </>,
  );
}

beforeEach(() => {
  for (const fn of Object.values(ipc)) fn.mockReset();
  ipc.agentsProviderStatus.mockImplementation(() => new Promise(() => {}));
  ipc.agentsModelsList.mockImplementation((provider: ProviderKind) =>
    Promise.resolve(
      provider === "openai"
        ? catalog("openai", [
            ["gpt-6.2-sol", "GPT-6.2 Sol"],
            ["gpt-6.2-luna", "GPT-6.2 Luna"],
          ])
        : catalog("anthropic", [
            ["claude-opus-5-5", "Claude Opus 5.5"],
            ["claude-sonnet-5-5", "Claude Sonnet 5.5"],
          ]),
    ),
  );
  core.invoke.mockReset();
  core.invoke.mockImplementation(() => Promise.resolve([]));
  useAgents.setState({ ...initialAgentsState, status: status(false) });
});
afterEach(cleanup);

describe("Settings › Model providers", () => {
  it("shows each provider's connection, key state, and model list, and offers no ChatGPT sign-in", async () => {
    renderSection();
    const anthropic = section("anthropic");
    expect(within(anthropic).getByText("Connected")).toBeTruthy();
    expect(within(anthropic).getByTestId("agents-key-status").textContent).toBe("Configured — stored in Windows Credential Manager");
    await waitFor(() => expect(within(anthropic).getByTestId("settings-provider-catalog").textContent).toContain("Listed by Anthropic just now. 2 models."));
    expect(within(anthropic).getByRole("button", { name: "Replace key" })).toBeTruthy();
    expect(within(anthropic).queryByLabelText(/API key/)).toBeNull();

    const openai = section("openai");
    expect(within(openai).getByText("Not connected")).toBeTruthy();
    expect(within(openai).getByTestId("agents-key-status").textContent).toBe("Not configured");
    expect(keyField("openai").type).toBe("password");
    expect(within(openai).getByTestId("settings-provider-catalog").textContent).toContain("Listed once an API key is saved");
    expect(within(openai).queryByRole("button", { name: "Remove" })).toBeNull();
    expect(ipc.agentsModelsList).not.toHaveBeenCalledWith("openai", expect.anything());
    // Sign-in with ChatGPT was never delivered: one muted line, no control.
    expect(within(openai).getByText("Sign in with ChatGPT is not available")).toBeTruthy();
    expect(screen.queryByRole("button", { name: /ChatGPT/ })).toBeNull();
    expect(screen.getByText(/never shown/).textContent).toContain("Windows Credential Manager");
    // The project permissions moved here.
    await screen.findByTestId("settings-project-permissions-empty");
  });

  it("takes the key out of the form before saving it, and back only if saving fails", async () => {
    let finish: (value: ProviderStatus) => void = () => {};
    ipc.agentsProviderSetKey.mockImplementation(() => new Promise((resolve) => (finish = resolve)));
    renderSection();
    fireEvent.change(keyField("openai"), { target: { value: KEY } });
    fireEvent.click(within(section("openai")).getByRole("button", { name: "Save key" }));
    // The provider event can report "Connected" before the call returns: by
    // then the key must already be gone from the form and the DOM.
    expect(ipc.agentsProviderSetKey).toHaveBeenCalledWith(KEY, "openai");
    expect(keyField("openai").value).toBe("");
    expect(document.documentElement.outerHTML.includes(KEY)).toBe(false);
    await act(async () => finish(status(true, {}, 2)));
    await within(section("openai")).findByText("OpenAI API key saved to the credential store.");
    expect(within(section("openai")).getByText("Connected")).toBeTruthy();
    expect(within(section("openai")).queryByLabelText(/API key/)).toBeNull();
    // The new connection lists its models.
    await waitFor(() => expect(ipc.agentsModelsList).toHaveBeenCalledWith("openai", false));
    expect(document.documentElement.outerHTML.includes(KEY)).toBe(false);
  });

  it("gives the key back with the reason when the credential store refuses it", async () => {
    ipc.agentsProviderSetKey.mockRejectedValue(new Error("credential store unavailable"));
    renderSection();
    fireEvent.change(keyField("openai"), { target: { value: KEY } });
    fireEvent.click(within(section("openai")).getByRole("button", { name: "Save key" }));
    await waitFor(() => expect(keyField("openai").value).toBe(KEY));
    expect(within(section("openai")).getByTestId("agents-settings-error").textContent).toContain("credential store unavailable");
  });

  it("replaces a configured key for that connection only", async () => {
    ipc.agentsProviderSetKey.mockResolvedValue(status(false));
    renderSection();
    const anthropic = section("anthropic");
    fireEvent.click(within(anthropic).getByRole("button", { name: "Replace key" }));
    expect(within(anthropic).getByLabelText("New API key")).toBeTruthy();
    fireEvent.change(keyField("anthropic"), { target: { value: KEY } });
    await act(async () => {
      fireEvent.click(within(anthropic).getByRole("button", { name: "Save key" }));
    });
    expect(ipc.agentsProviderSetKey).toHaveBeenCalledWith(KEY, "anthropic");
    await within(anthropic).findByText("Anthropic API key saved to the credential store.");
    expect(within(anthropic).queryByLabelText(/API key/)).toBeNull();
  });

  it("asks before removing the stored key; Escape keeps it", async () => {
    ipc.agentsProviderClearKey.mockResolvedValue(status(false, { connections: [] , configured: false }));
    renderSection();
    const remove = () => within(section("anthropic")).getByRole("button", { name: "Remove" });
    fireEvent.click(remove());
    const alert = await screen.findByRole("alertdialog");
    expect(alert.textContent).toContain("Remove the API key?");
    expect(alert.textContent).toContain("Windows Credential Manager");
    expect(screen.getByRole("button", { name: "Keep key" })).toBeTruthy();
    fireEvent.keyDown(alert, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
    expect(ipc.agentsProviderClearKey).not.toHaveBeenCalled();

    fireEvent.click(remove());
    fireEvent.click(await screen.findByRole("button", { name: "Remove key" }));
    await waitFor(() => expect(ipc.agentsProviderClearKey).toHaveBeenCalledWith("anthropic"));
    await within(section("anthropic")).findByText("Anthropic API key removed.");
    expect(within(section("anthropic")).getByText("Not connected")).toBeTruthy();
  });

  it("shows a credential-store error on its connection", () => {
    useAgents.setState({
      status: status(false, {
        connections: [
          { provider: "anthropic", auth: "apiKey", configured: false, credentialError: "The credential store is locked.", context: null, version: 1 },
          { provider: "openai", auth: "apiKey", configured: false, credentialError: null, context: null, version: 1 },
        ],
      }),
    });
    renderSection();
    const anthropic = section("anthropic");
    expect(within(anthropic).getAllByText("Credential storage error").length).toBeGreaterThan(0);
    expect(within(anthropic).getByText("The credential store is locked.")).toBeTruthy();
    expect(within(section("openai")).queryByText("Credential storage error")).toBeNull();
  });

  it("saves OpenAI's organization and project, and shows the service's refusal", async () => {
    useAgents.setState({ status: status(true) });
    ipc.agentsProviderUpdate.mockRejectedValueOnce(new Error("The organization ID should look like org-…"));
    renderSection();
    const openai = section("openai");
    const save = within(openai).getByRole("button", { name: "Save organization and project" });
    expect((save as HTMLButtonElement).disabled).toBe(true);
    expect((within(openai).getByTestId("agents-openai-project").querySelector("input") as HTMLInputElement).value).toBe("proj_abc");
    fireEvent.change(within(openai).getByTestId("agents-openai-organization").querySelector("input")!, { target: { value: "acme" } });
    await act(async () => {
      fireEvent.click(save);
    });
    expect(within(openai).getByTestId("agents-settings-error").textContent).toContain("should look like org-");

    const next = status(true);
    next.settings = { ...next.settings, openai: { organization: "org-acme", project: "proj_abc" } };
    ipc.agentsProviderUpdate.mockResolvedValueOnce(next);
    fireEvent.change(within(openai).getByTestId("agents-openai-organization").querySelector("input")!, { target: { value: " org-acme " } });
    await act(async () => {
      fireEvent.click(save);
    });
    expect(ipc.agentsProviderUpdate).toHaveBeenLastCalledWith({
      provider: "anthropic",
      model: "claude-opus-5-5",
      effort: "high",
      consent: "scopeContent",
      openai: { organization: "org-acme", project: "proj_abc" },
    });
    await within(openai).findByText("OpenAI organization and project saved.");
  });

  it("refreshes a connection's model list and shows a stale list with its reason", async () => {
    renderSection();
    const anthropic = section("anthropic");
    await waitFor(() => expect(within(anthropic).getByTestId("settings-provider-catalog").textContent).toContain("Listed by Anthropic"));
    ipc.agentsModelsList.mockResolvedValueOnce(
      catalog("anthropic", [["claude-opus-5-5", "Claude Opus 5.5"]], { state: "stale", fetchedAtMs: Date.now() - 5 * 60_000, error: "Could not reach Anthropic." }),
    );
    await act(async () => {
      fireEvent.click(within(anthropic).getByRole("button", { name: "Refresh model list" }));
    });
    expect(ipc.agentsModelsList).toHaveBeenLastCalledWith("anthropic", true);
    await waitFor(() => expect(within(anthropic).getByTestId("settings-provider-catalog").textContent).toContain("May be out of date"));
    expect(within(anthropic).getByTestId("settings-provider-catalog").textContent).toContain("Fetched 5 min ago. Could not reach Anthropic.");
  });

  it("chooses the default model and effort through the model menu and the content sent", async () => {
    useAgents.setState({ status: status(true) });
    const saved = status(true);
    saved.settings = { ...saved.settings, model: "claude-sonnet-5-5", effort: "high" };
    ipc.agentsProviderUpdate.mockResolvedValue(saved);
    renderSection();
    const defaults = screen.getByTestId("agents-default");
    const trigger = within(defaults).getByTestId("agents-default-model");
    await waitFor(() => expect(trigger.textContent).toBe("Claude Opus 5.5 · High"));
    fireEvent.click(trigger);
    const menu = await screen.findByTestId("agents-default-menu");
    // The default itself is edited here: no "Use default" footer.
    expect(within(menu).queryByRole("button", { name: /^Use default/ })).toBeNull();
    const sonnet = await waitFor(() => within(menu).getAllByRole("option").find((o) => o.getAttribute("data-model-id") === "claude-sonnet-5-5")!);
    await act(async () => {
      fireEvent.click(sonnet);
    });
    expect(ipc.agentsProviderUpdate).toHaveBeenLastCalledWith({
      provider: "anthropic",
      model: "claude-sonnet-5-5",
      effort: "high",
      consent: "scopeContent",
      openai: { organization: null, project: "proj_abc" },
    });
    await within(defaults).findByText("Default model saved.");
    await waitFor(() => expect(trigger.textContent).toBe("Claude Sonnet 5.5 · High"));
  });

  it("follows provider events: another view's key change shows here and lists that connection", async () => {
    renderSection();
    expect(within(section("openai")).getByText("Not connected")).toBeTruthy();
    const effect = reduceEvent(useAgents.getState(), { kind: "providerChanged", status: status(true, {}, 2) });
    act(() => useAgents.setState(effect.patch));
    expect(within(section("openai")).getByText("Connected")).toBeTruthy();
    await waitFor(() => expect(ipc.agentsModelsList).toHaveBeenCalledWith("openai", false));
    await waitFor(() => expect(within(section("openai")).getByTestId("settings-provider-catalog").textContent).toContain("Listed by OpenAI"));
  });
});
