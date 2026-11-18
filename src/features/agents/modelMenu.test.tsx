// The model menu: provider groups, search across groups, saved/unlisted and
// custom choices, unconnected providers, per-group list states, effort stops
// per model (fixed and none), per-model effort memory, the unverified
// acknowledgement, the running header, the footer, and that choosing never
// sends anything.
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { Effort } from "../../bindings/Effort";
import type { ModelCatalog } from "../../bindings/ModelCatalog";
import type { ModelDescriptor } from "../../bindings/ModelDescriptor";
import type { ProviderChoice } from "../../bindings/ProviderChoice";
import type { ProviderKind } from "../../bindings/ProviderKind";
import type { ProviderStatus } from "../../bindings/ProviderStatus";
import type { ModelMenuProps } from "./ModelMenu";

const ipc = vi.hoisted(() => ({
  agentsModelsList: vi.fn(),
  agentsProviderUpdate: vi.fn(),
  agentsProviderSetKey: vi.fn(),
  agentsTaskCreate: vi.fn(),
  agentsTaskSend: vi.fn(),
}));
vi.mock("../../lib/ipc", () => ({
  commands: ipc,
  events: { agents: vi.fn(), agentsResync: vi.fn() },
  errorMessage: (e: unknown) => (e instanceof Error ? e.message : String(e)),
}));

const { ModelMenu, choiceSummary } = await import("./ModelMenu");
const { resetRememberedEfforts, filterModels } = await import("./models");

const ALL: Effort[] = ["low", "medium", "high", "xhigh", "max"];

function model(provider: ProviderKind, id: string, overrides: Partial<ModelDescriptor> = {}): ModelDescriptor {
  return {
    provider,
    id,
    displayName: id,
    lifecycle: { kind: "active" },
    agent: { kind: "supported" },
    capabilities: { tools: "yes", images: "yes", pdf: "yes", efforts: ALL, contextTokens: null, maxOutputTokens: null },
    source: "catalog",
    createdAtMs: 1,
    ...overrides,
  };
}

function catalog(provider: ProviderKind, models: ModelDescriptor[], overrides: Partial<ModelCatalog> = {}): ModelCatalog {
  return {
    provider,
    state: "fresh",
    fetchedAtMs: Date.now(),
    error: null,
    models,
    suggested: null,
    suggestedReason: null,
    metadataChecked: "2026-11-17",
    ...overrides,
  };
}

function providerStatus(openai = true, overrides: Partial<ProviderStatus> = {}, version = 1): ProviderStatus {
  return {
    settings: { provider: "anthropic", model: "claude-opus-5-5", effort: "medium", consent: "scopeContent" },
    configured: true,
    connections: [
      { provider: "anthropic", auth: "apiKey", configured: true, credentialError: null, context: null, version: 1 },
      { provider: "openai", auth: "apiKey", configured: openai, credentialError: null, context: null, version },
    ],
    credentialStore: "Windows Credential Manager",
    credentialError: null,
    scriptedAvailable: false,
    ...overrides,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => (resolve = res));
  return { promise, resolve };
}

const CLAUDE = [
  model("anthropic", "claude-opus-5-5", { displayName: "Claude Opus 5.5" }),
  model("anthropic", "claude-sonnet-5-5", { displayName: "Claude Sonnet 5.5", capabilities: { ...model("anthropic", "x").capabilities, efforts: ["low", "medium", "high"] } }),
  model("anthropic", "claude-3-haiku", {
    displayName: "Claude Haiku 3",
    agent: { kind: "unverified", reason: "Older Claude model: not verified." },
    capabilities: { tools: "yes", images: "yes", pdf: "unknown", efforts: [], contextTokens: null, maxOutputTokens: null },
  }),
  model("anthropic", "claude-2-1", {
    displayName: "Claude 2.1",
    agent: { kind: "unsupported", reason: "Retired: requests naming this model fail." },
    lifecycle: { kind: "retired" },
  }),
];
const GPT = [
  model("openai", "gpt-6.2-sol", { displayName: "GPT-6.2 Sol", capabilities: { ...model("openai", "x").capabilities, efforts: ["high"] } }),
  model("openai", "gpt-6.2-luna", { displayName: "GPT-6.2 Luna", capabilities: { ...model("openai", "x").capabilities, efforts: ["low", "medium", "high"] } }),
];

function lists(overrides: Partial<Record<ProviderKind, ModelCatalog>> = {}) {
  ipc.agentsModelsList.mockImplementation((provider: ProviderKind) =>
    Promise.resolve(overrides[provider] ?? (provider === "anthropic" ? catalog("anthropic", CLAUDE, { suggested: "claude-opus-5-5" }) : catalog("openai", GPT))),
  );
}

/** A controlled menu (the caller's draft) that records every change. */
function Harness({ initial, ...props }: Partial<ModelMenuProps> & { initial: ProviderChoice | null; status: ProviderStatus | null }) {
  const [value, setValue] = useState(initial);
  return (
    <>
      <ModelMenu
        value={value}
        onChange={(next) => {
          changes.push(next);
          setValue(next);
        }}
        onManageProviders={props.onManageProviders ?? (() => {})}
        {...props}
        status={props.status}
      />
      <output data-testid="value">{value ? `${value.provider}|${value.model}|${value.effort}` : "default"}</output>
    </>
  );
}

let changes: (ProviderChoice | null)[] = [];
const valueText = () => screen.getByTestId("value").textContent;
const group = (provider: ProviderKind) => screen.getAllByTestId("model-group").find((g) => g.dataset.provider === provider)!;
const optionIds = (provider: ProviderKind) => within(group(provider)).queryAllByRole("option").map((o) => o.getAttribute("data-model-id"));
const option = (id: string) => screen.getAllByRole("option").find((o) => o.getAttribute("data-model-id") === id)!;
const stops = () => [...document.querySelectorAll(".ui-slider__stop")].map((s) => s.textContent);
const clickStop = (label: string) => fireEvent.click([...document.querySelectorAll(".ui-slider__stop")].find((s) => s.textContent === label)!);

beforeEach(() => {
  for (const fn of Object.values(ipc)) fn.mockReset();
  changes = [];
  resetRememberedEfforts();
});
afterEach(cleanup);

describe("model menu", () => {
  it("groups every provider's models under its name, with the exact ID, capabilities, and dashed support badges", async () => {
    lists();
    render(<Harness initial={null} status={providerStatus()} />);
    await waitFor(() => expect(optionIds("openai")).toEqual(["gpt-6.2-sol", "gpt-6.2-luna"]));
    expect(screen.getAllByTestId("model-group").map((g) => g.getAttribute("aria-label"))).toEqual(["Anthropic", "OpenAI"]);
    expect(optionIds("anthropic")).toEqual(["claude-opus-5-5", "claude-sonnet-5-5", "claude-3-haiku", "claude-2-1"]);
    expect(within(group("anthropic")).getByRole("listbox", { name: "Anthropic models" })).toBeTruthy();
    // The default is shown as chosen while the draft inherits it.
    expect(option("claude-opus-5-5").getAttribute("aria-selected")).toBe("true");
    const opus = option("claude-opus-5-5");
    expect(opus.querySelector("code")?.textContent).toBe("claude-opus-5-5");
    expect(opus.textContent).toContain("Images · PDF · Effort");
    const suggested = within(opus).getByText("Suggested");
    expect(suggested.closest(".ui-badge")?.getAttribute("data-dashed")).toBe("true");
    expect(within(option("claude-3-haiku")).getByText("Unverified").closest(".ui-badge")?.getAttribute("data-dashed")).toBe("true");
    const retired = option("claude-2-1");
    expect(retired.getAttribute("aria-disabled")).toBe("true");
    expect(retired.textContent).toContain("Retired: requests naming this model fail.");
    expect(within(group("anthropic")).getByTestId("model-group-state").textContent).toBe("Listed just now");
    // An unsupported row cannot be chosen.
    fireEvent.click(retired);
    expect(changes).toEqual([]);
  });

  it("searches every group and keeps each result under its provider's name", async () => {
    lists();
    render(<Harness initial={null} status={providerStatus()} />);
    await waitFor(() => expect(optionIds("openai")).toHaveLength(2));
    const search = screen.getByRole("searchbox", { name: "Search models" });
    fireEvent.change(search, { target: { value: "sol" } });
    expect(screen.getAllByTestId("model-group").map((g) => g.dataset.provider)).toEqual(["openai"]);
    expect(optionIds("openai")).toEqual(["gpt-6.2-sol"]);
    fireEvent.change(search, { target: { value: "5" } });
    expect(screen.getAllByTestId("model-group").map((g) => g.dataset.provider)).toEqual(["anthropic"]);
    fireEvent.change(search, { target: { value: "high" } });
    expect(screen.getByText("No models match “high”.")).toBeTruthy();
    // The provider name matches too.
    fireEvent.change(search, { target: { value: "openai luna" } });
    expect(optionIds("openai")).toEqual(["gpt-6.2-luna"]);
    expect(filterModels([...CLAUDE, ...GPT], "o").length).toBe(6);
  });

  it("keeps a saved model the catalog does not list, marked and chosen, never replaced", async () => {
    lists();
    render(<Harness initial={{ provider: "anthropic", model: "claude-legacy-7", effort: "high" }} status={providerStatus()} />);
    await waitFor(() => expect(optionIds("anthropic")[0]).toBe("claude-legacy-7"));
    const first = option("claude-legacy-7");
    expect(first.getAttribute("aria-selected")).toBe("true");
    expect(first.textContent).toContain("Saved, not listed");
    expect(first.textContent).toContain("Not listed by this connection. Exposition checks it when a task starts.");
    // Unknown levels are offered honestly, all of them.
    expect(stops()).toEqual(["Low", "Medium", "High", "Extra high", "Max"]);
    expect(screen.getByText(/does not know which levels claude-legacy-7 accepts/)).toBeTruthy();
    expect(valueText()).toBe("anthropic|claude-legacy-7|high");
    expect(changes).toEqual([]);
  });

  it("shows an unconnected provider with no models and a link to Settings", async () => {
    lists();
    const manage = vi.fn();
    render(<Harness initial={{ provider: "openai", model: "gpt-6.2-sol", effort: "high" }} status={providerStatus(false)} onManageProviders={manage} />);
    await waitFor(() => expect(optionIds("anthropic")).toHaveLength(4));
    const openai = group("openai");
    expect(within(openai).getByText("Not connected")).toBeTruthy();
    // Only the chosen model is shown (it is the draft's), never a fabricated list.
    expect(optionIds("openai")).toEqual(["gpt-6.2-sol"]);
    expect(ipc.agentsModelsList).not.toHaveBeenCalledWith("openai", expect.anything());
    fireEvent.click(within(openai).getByRole("button", { name: "Configure in Settings" }));
    expect(manage).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByRole("button", { name: "Manage providers…" }));
    expect(manage).toHaveBeenCalledTimes(2);
  });

  it("offers exactly the chosen model's effort stops: several, one fixed value, or none", async () => {
    lists();
    render(<Harness initial={{ provider: "anthropic", model: "claude-opus-5-5", effort: "max" }} status={providerStatus()} />);
    await waitFor(() => expect(stops()).toEqual(["Low", "Medium", "High", "Extra high", "Max"]));
    expect(screen.getByRole("slider", { name: "Effort" }).getAttribute("aria-valuetext")).toBe("Max");
    // Three levels; Max is not one of them, so Medium.
    fireEvent.click(option("claude-sonnet-5-5"));
    expect(valueText()).toBe("anthropic|claude-sonnet-5-5|medium");
    expect(stops()).toEqual(["Low", "Medium", "High"]);
    // One level: a fixed value, no slider.
    fireEvent.click(option("gpt-6.2-sol"));
    expect(valueText()).toBe("openai|gpt-6.2-sol|high");
    expect(screen.queryByRole("slider")).toBeNull();
    expect(screen.getByTestId("model-effort").textContent).toContain("High");
    // Back to three levels; High is kept.
    fireEvent.click(option("gpt-6.2-luna"));
    clickStop("Low");
    expect(valueText()).toBe("openai|gpt-6.2-luna|low");
  });

  it("remembers the effort chosen for each model in the draft", async () => {
    lists();
    render(<Harness initial={{ provider: "anthropic", model: "claude-opus-5-5", effort: "medium" }} status={providerStatus()} />);
    await waitFor(() => expect(stops()).toHaveLength(5));
    clickStop("Extra high");
    expect(valueText()).toBe("anthropic|claude-opus-5-5|xhigh");
    fireEvent.click(option("gpt-6.2-luna"));
    expect(valueText()).toBe("openai|gpt-6.2-luna|medium");
    clickStop("Low");
    fireEvent.click(option("claude-opus-5-5"));
    expect(valueText()).toBe("anthropic|claude-opus-5-5|xhigh");
    fireEvent.click(option("gpt-6.2-luna"));
    expect(valueText()).toBe("openai|gpt-6.2-luna|low");
  });

  it("says when a model has no effort setting", async () => {
    lists();
    render(<Harness initial={{ provider: "anthropic", model: "claude-3-haiku", effort: "medium" }} status={providerStatus()} />);
    await screen.findByText("Effort is managed by this model.");
    expect(screen.queryByRole("slider")).toBeNull();
  });

  it("asks for a visible confirmation before an unverified model reaches the draft", async () => {
    lists();
    render(<Harness initial={{ provider: "anthropic", model: "claude-opus-5-5", effort: "medium" }} status={providerStatus()} />);
    await waitFor(() => expect(optionIds("anthropic")).toHaveLength(4));
    fireEvent.click(option("claude-3-haiku"));
    expect(changes).toEqual([]);
    expect(option("claude-3-haiku").getAttribute("aria-selected")).toBe("true");
    const banner = screen.getByTestId("model-unverified");
    expect(banner.textContent).toContain("Older Claude model: not verified.");
    fireEvent.click(within(banner).getByRole("checkbox", { name: "Use this unverified model" }));
    expect(valueText()).toBe("anthropic|claude-3-haiku|medium");
    expect(screen.queryByTestId("model-unverified")).toBeNull();
    expect(screen.getByTestId("model-unverified-chosen").textContent).toContain("not verified");
    // Once acknowledged, choosing it again needs no second confirmation.
    fireEvent.click(option("claude-opus-5-5"));
    fireEvent.click(option("claude-3-haiku"));
    expect(valueText()).toBe("anthropic|claude-3-haiku|medium");
  });

  it("accepts a custom model ID for the chosen provider and refuses malformed IDs", async () => {
    lists();
    render(<Harness initial={{ provider: "openai", model: "gpt-6.2-luna", effort: "medium" }} status={providerStatus()} />);
    await waitFor(() => expect(optionIds("openai")).toHaveLength(2));
    fireEvent.click(screen.getByRole("button", { name: "Custom model ID…" }));
    const field = screen.getByRole("textbox", { name: "Exact model ID" });
    fireEvent.change(field, { target: { value: "bad id" } });
    fireEvent.click(screen.getByRole("button", { name: "Use this ID" }));
    expect(screen.getByText(/letters, digits/)).toBeTruthy();
    fireEvent.change(field, { target: { value: "ft:gpt-6.2-luna:acme:42" } });
    fireEvent.click(screen.getByRole("button", { name: "Use this ID" }));
    expect(valueText()).toBe("openai|ft:gpt-6.2-luna:acme:42|medium");
    const row = option("ft:gpt-6.2-luna:acme:42");
    expect(row.closest("[data-testid=model-group]")?.getAttribute("data-provider")).toBe("openai");
    expect(row.textContent).toContain("Custom ID, not listed");
  });

  it("shows each group's list state: loading, stale with refresh failed, and an error with Retry", async () => {
    const slow = deferred<ModelCatalog>();
    ipc.agentsModelsList.mockImplementation((provider: ProviderKind) =>
      provider === "anthropic"
        ? Promise.resolve(catalog("anthropic", CLAUDE, { state: "stale", fetchedAtMs: Date.now() - 5 * 60_000, error: "Could not reach Anthropic." }))
        : slow.promise,
    );
    render(<Harness initial={null} status={providerStatus()} />);
    expect(within(group("openai")).getByText("Loading models…")).toBeTruthy();
    await waitFor(() => expect(within(group("anthropic")).getByTestId("model-group-state").textContent).toContain("Listed 5 min ago; refresh failed"));
    expect(within(group("anthropic")).getByText("Could not reach Anthropic.")).toBeTruthy();
    expect(optionIds("anthropic")).toHaveLength(4);

    await act(async () => {
      fireEvent.click(within(group("anthropic")).getByRole("button", { name: "Refresh Anthropic models" }));
    });
    expect(ipc.agentsModelsList).toHaveBeenLastCalledWith("anthropic", true);
    cleanup();

    ipc.agentsModelsList.mockReset();
    ipc.agentsModelsList.mockImplementation((provider: ProviderKind) =>
      provider === "openai" ? Promise.reject(new Error("IPC unavailable")) : Promise.resolve(catalog("anthropic", CLAUDE)),
    );
    render(<Harness initial={null} status={providerStatus()} />);
    await within(group("openai")).findByText("IPC unavailable");
    expect(optionIds("openai")).toEqual([]);
    ipc.agentsModelsList.mockImplementation((provider: ProviderKind) => Promise.resolve(provider === "openai" ? catalog("openai", GPT) : catalog("anthropic", CLAUDE)));
    await act(async () => {
      fireEvent.click(within(group("openai")).getByRole("button", { name: "Retry listing OpenAI models" }));
    });
    await waitFor(() => expect(optionIds("openai")).toHaveLength(2));
  });

  it("reads 'For the next message' while a task runs and names the running model apart", async () => {
    lists();
    render(
      <Harness
        initial={{ provider: "openai", model: "gpt-6.2-luna", effort: "low" }}
        status={providerStatus()}
        running={{ provider: "anthropic", model: "claude-opus-5-5", effort: "high" }}
      />,
    );
    const header = screen.getByTestId("model-running");
    expect(within(header).getByText("For the next message")).toBeTruthy();
    await waitFor(() => expect(header.textContent).toContain("Running: Claude Opus 5.5 · High"));
  });

  it("changes the draft only: no request is made, and Use default returns to the inherited choice", async () => {
    lists();
    render(<Harness initial={{ provider: "openai", model: "gpt-6.2-luna", effort: "low" }} status={providerStatus()} />);
    await waitFor(() => expect(optionIds("openai")).toHaveLength(2));
    const listed = ipc.agentsModelsList.mock.calls.length;
    fireEvent.click(option("claude-sonnet-5-5"));
    clickStop("High");
    fireEvent.click(option("gpt-6.2-sol"));
    expect(ipc.agentsModelsList).toHaveBeenCalledTimes(listed);
    expect(ipc.agentsProviderUpdate).not.toHaveBeenCalled();
    expect(ipc.agentsProviderSetKey).not.toHaveBeenCalled();
    expect(ipc.agentsTaskCreate).not.toHaveBeenCalled();
    expect(ipc.agentsTaskSend).not.toHaveBeenCalled();

    const useDefault = screen.getByRole("button", { name: "Use default (Claude Opus 5.5 · Medium)" });
    fireEvent.click(useDefault);
    expect(changes.at(-1)).toBeNull();
    expect(valueText()).toBe("default");
    expect((screen.getByRole("button", { name: /^Use default/ }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("moves through every group with the keyboard and skips unavailable models", async () => {
    lists();
    render(<Harness initial={{ provider: "anthropic", model: "claude-opus-5-5", effort: "medium" }} status={providerStatus()} />);
    await waitFor(() => expect(optionIds("openai")).toHaveLength(2));
    fireEvent.keyDown(screen.getByRole("searchbox"), { key: "ArrowDown" });
    expect(document.activeElement?.getAttribute("data-model-id")).toBe("claude-opus-5-5");
    fireEvent.keyDown(document.activeElement!, { key: "ArrowDown" });
    fireEvent.keyDown(document.activeElement!, { key: "ArrowDown" });
    // Claude 2.1 (unsupported) is skipped: the next one is OpenAI's first.
    fireEvent.keyDown(document.activeElement!, { key: "ArrowDown" });
    expect(document.activeElement?.getAttribute("data-model-id")).toBe("gpt-6.2-sol");
    fireEvent.keyDown(document.activeElement!, { key: "Enter" });
    expect(valueText()).toBe("openai|gpt-6.2-sol|high");
  });

  it("lists a connection again when its key changes (another account) and drops the late answer", async () => {
    const first = deferred<ModelCatalog>();
    ipc.agentsModelsList.mockImplementation((provider: ProviderKind) =>
      provider === "openai" ? first.promise : Promise.resolve(catalog("anthropic", CLAUDE)),
    );
    const props = { value: null, onChange: () => {}, onManageProviders: () => {} };
    const { rerender } = render(<ModelMenu {...props} status={providerStatus(true, {}, 1)} />);
    ipc.agentsModelsList.mockImplementation((provider: ProviderKind) =>
      Promise.resolve(provider === "openai" ? catalog("openai", [GPT[0]!]) : catalog("anthropic", CLAUDE)),
    );
    rerender(<ModelMenu {...props} status={providerStatus(true, {}, 2)} />);
    await waitFor(() => expect(optionIds("openai")).toEqual(["gpt-6.2-sol"]));
    await act(async () => first.resolve(catalog("openai", [model("openai", "only-on-account-a")])));
    expect(optionIds("openai")).toEqual(["gpt-6.2-sol"]);
  });

  it("summarizes a choice by its friendly name and effort", () => {
    expect(choiceSummary({ provider: "anthropic", model: "claude-opus-5-5", effort: "high" }, CLAUDE[0]!)).toBe("Claude Opus 5.5 · High");
    expect(choiceSummary({ provider: "anthropic", model: "claude-3-haiku", effort: "high" }, CLAUDE[2]!)).toBe("Claude Haiku 3");
    expect(choiceSummary({ provider: "openai", model: "gpt-x", effort: "low" }, null)).toBe("gpt-x · Low");
    expect(choiceSummary({ provider: "scripted", model: "scripted", effort: "low" }, null)).toBe("Scripted provider — development only");
  });
});
