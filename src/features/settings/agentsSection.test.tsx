// Settings › Agents: the default for new tasks and each provider connection's
// state (never a key).
import { cleanup, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ProviderStatus } from "../../bindings/ProviderStatus";

const ipc = vi.hoisted(() => ({ agentsProviderStatus: vi.fn(() => new Promise(() => {})) }));
const core = vi.hoisted(() => ({ invoke: vi.fn(() => Promise.resolve([])) }));
vi.mock("@tauri-apps/api/core", () => core);
vi.mock("../../lib/ipc", () => ({
  commands: ipc,
  events: {
    agents: vi.fn(() => new Promise(() => {})),
    agentsResync: vi.fn(),
    workspace: vi.fn(),
    documents: vi.fn(),
  },
  errorMessage: (e: unknown) => String(e),
}));

const { AgentsSection } = await import("./AgentsSection");
const { initialAgentsState, useAgents } = await import("../agents/store");

const status: ProviderStatus = {
  settings: { provider: "openai", model: "gpt-6.2-sol", effort: "high", consent: "scopeContent", openai: { organization: null, project: "proj_abc" } },
  configured: true,
  connections: [
    { provider: "anthropic", auth: "apiKey", configured: false, credentialError: null, context: null, version: 1 },
    { provider: "openai", auth: "apiKey", configured: true, credentialError: null, context: "Project proj_abc", version: 1 },
  ],
  credentialStore: "Windows Credential Manager",
  credentialError: null,
  scriptedAvailable: false,
};

afterEach(cleanup);

describe("Settings › Agents", () => {
  it("shows the default provider and model, and each connection's state with its context", () => {
    useAgents.setState({ ...initialAgentsState, status });
    render(<AgentsSection />);
    const defaults = screen.getByTestId("agents-default");
    expect(defaults.textContent).toContain("OpenAI");
    expect(within(screen.getByTestId("agents-default-model")).getByText("gpt-6.2-sol")).toBeTruthy();
    expect(screen.getByTestId("agents-default-model").textContent).toContain("High effort");
    const openai = screen.getByTestId("agents-connection-openai");
    expect(openai.textContent).toContain("OpenAI API key");
    expect(openai.textContent).toContain("Configured");
    expect(openai.textContent).toContain("Used by new tasks. Project proj_abc.");
    const anthropic = screen.getByTestId("agents-connection-anthropic");
    expect(anthropic.textContent).toContain("Not configured");
    expect(anthropic.textContent).toContain("Add a key in Provider settings.");
    expect(screen.getByTestId("agents-key").textContent).toContain("never shown");
    expect(screen.queryByText(/ChatGPT/)).toBeNull();
  });
});
