// The model picker: catalog states (loading, empty, error, stale, unavailable),
// search, a saved model the catalog does not list, the unverified
// acknowledgement, unsupported rows, effort levels per model, custom IDs,
// keyboard use, and discarding late results after the connection changed.
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ModelCatalog } from "../../bindings/ModelCatalog";
import type { ModelDescriptor } from "../../bindings/ModelDescriptor";
import type { ProviderChoice } from "../../bindings/ProviderChoice";
import type { ProviderKind } from "../../bindings/ProviderKind";
import type { ProviderStatus } from "../../bindings/ProviderStatus";

const ipc = vi.hoisted(() => ({ agentsModelsList: vi.fn() }));
vi.mock("../../lib/ipc", () => ({
  commands: ipc,
  events: { agents: vi.fn(), agentsResync: vi.fn() },
  errorMessage: (e: unknown) => (e instanceof Error ? e.message : String(e)),
}));

const { ModelPicker } = await import("./ModelPicker");
const { capabilityHints, catalogRows, choiceProblem, connectionOf, effortFor, filterModels, modelBadges } = await import("./models");

function model(id: string, overrides: Partial<ModelDescriptor> = {}): ModelDescriptor {
  return {
    provider: id.startsWith("claude") ? "anthropic" : "openai",
    id,
    displayName: id,
    lifecycle: { kind: "active" },
    agent: { kind: "supported" },
    capabilities: { tools: "yes", images: "yes", pdf: "yes", efforts: ["low", "medium", "high", "xhigh", "max"], contextTokens: null, maxOutputTokens: null },
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

function providerStatus(overrides: Partial<ProviderStatus> = {}): ProviderStatus {
  return {
    settings: { provider: "anthropic", model: "claude-opus-5-5", effort: "medium", consent: "scopeContent" },
    configured: true,
    connections: [
      { provider: "anthropic", auth: "apiKey", configured: true, credentialError: null, context: null, version: 1 },
      { provider: "openai", auth: "apiKey", configured: true, credentialError: null, context: null, version: 1 },
    ],
    credentialStore: "Windows Credential Manager",
    credentialError: null,
    scriptedAvailable: false,
    ...overrides,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const CLAUDE = [
  model("claude-opus-5-5", { displayName: "Claude Opus 5.5" }),
  model("claude-sonnet-5-5", { displayName: "Claude Sonnet 5.5" }),
  model("claude-3-haiku", {
    displayName: "Claude Haiku 3",
    agent: { kind: "unverified", reason: "Older Claude model: not verified." },
    capabilities: { tools: "yes", images: "yes", pdf: "unknown", efforts: [], contextTokens: null, maxOutputTokens: null },
  }),
  model("claude-2-1", { displayName: "Claude 2.1", agent: { kind: "unsupported", reason: "Retired: requests naming this model fail." }, lifecycle: { kind: "retired" } }),
];

/** A controlled picker that records every change and the reported problem. */
function Harness({ initial, status, onProblem }: { initial: ProviderChoice; status: ProviderStatus; onProblem?: (p: string | null) => void }) {
  const [value, setValue] = useState(initial);
  return (
    <>
      <ModelPicker value={value} onChange={setValue} status={status} onProblemChange={onProblem} />
      <output data-testid="value">{`${value.provider}|${value.model}|${value.effort}`}</output>
    </>
  );
}

const valueText = () => screen.getByTestId("value").textContent;

beforeEach(() => {
  ipc.agentsModelsList.mockReset();
});
afterEach(cleanup);

describe("model helpers", () => {
  it("filters by name or exact ID, keeps unlisted choices, and states only known capabilities", () => {
    expect(filterModels(CLAUDE, "sonnet").map((m) => m.id)).toEqual(["claude-sonnet-5-5"]);
    expect(filterModels(CLAUDE, "CLAUDE 5-5").map((m) => m.id)).toEqual(["claude-opus-5-5", "claude-sonnet-5-5"]);
    const rows = catalogRows(catalog("anthropic", CLAUDE), { provider: "anthropic", model: "claude-legacy-1", effort: "high" });
    expect(rows.rows[0]?.id).toBe("claude-legacy-1");
    expect(rows.unlisted).toBe(true);
    expect(modelBadges(rows.selected!, null).map((b) => b.label)).toEqual(["Saved, not listed"]);
    expect(capabilityHints(rows.selected!)).toEqual([]);
    expect(capabilityHints(model("x", { capabilities: { tools: "yes", images: "yes", pdf: "no", efforts: [], contextTokens: 200_000, maxOutputTokens: null } }))).toEqual([
      "Images",
      "200k context",
    ]);
    expect(modelBadges(CLAUDE[3]!, null).map((b) => b.label)).toEqual(["Unavailable", "Retired"]);
    expect(modelBadges(CLAUDE[0]!, "claude-opus-5-5").map((b) => b.label)).toEqual(["Suggested"]);
  });

  it("keeps a supported effort, prefers Medium otherwise, and explains unusable choices", () => {
    expect(effortFor("max", ["low", "medium", "high"])).toBe("medium");
    expect(effortFor("high", ["low", "medium", "high"])).toBe("high");
    expect(effortFor("max", [])).toBe("max");
    expect(effortFor("max", ["low", "high"])).toBe("low");
    const choice = { provider: "anthropic" as const, model: "claude-3-haiku", effort: "medium" as const };
    expect(choiceProblem(choice, CLAUDE[2]!, false)).toMatch(/unverified/);
    expect(choiceProblem(choice, CLAUDE[2]!, true)).toBeNull();
    expect(choiceProblem({ ...choice, model: "" }, null, true)).toBe("Choose a model.");
    expect(choiceProblem({ ...choice, model: "claude-2-1" }, CLAUDE[3]!, true)).toMatch(/cannot run agents/);
  });

  it("reads each provider's connection, falling back to the default provider's status", () => {
    const legacy = providerStatus({ connections: [], configured: true });
    expect(connectionOf(legacy, "anthropic").configured).toBe(true);
    expect(connectionOf(legacy, "openai").configured).toBe(false);
    expect(connectionOf(providerStatus(), "openai").configured).toBe(true);
    expect(connectionOf(providerStatus({ scriptedAvailable: true }), "scripted").configured).toBe(true);
  });
});

describe("model picker", () => {
  it("loads the connection's models, shows friendly names with the exact ID and badges, and searches", async () => {
    const load = deferred<ModelCatalog>();
    ipc.agentsModelsList.mockReturnValue(load.promise);
    render(<Harness initial={{ provider: "anthropic", model: "claude-opus-5-5", effort: "medium" }} status={providerStatus()} />);
    expect(screen.getByText("Loading models…")).toBeTruthy();
    expect(ipc.agentsModelsList).toHaveBeenCalledWith("anthropic", false);
    await act(async () => load.resolve(catalog("anthropic", CLAUDE)));
    const list = screen.getByRole("listbox", { name: "Models" });
    const options = within(list).getAllByRole("option");
    expect(options.map((o) => o.getAttribute("aria-label"))).toEqual(["Claude Opus 5.5", "Claude Sonnet 5.5", "Claude Haiku 3", "Claude 2.1"]);
    expect(options[0]!.getAttribute("aria-selected")).toBe("true");
    expect(options[0]!.textContent).toContain("claude-opus-5-5");
    expect(options[0]!.textContent).toContain("Images · PDF · Effort");
    expect(options[2]!.textContent).toContain("Unverified");
    expect(options[3]!.textContent).toContain("Retired");
    expect(options[3]!.getAttribute("aria-disabled")).toBe("true");
    expect(screen.getByTestId("model-fresh").textContent).toMatch(/Listed by Anthropic just now\. Support information checked 2026-11-17\./);

    fireEvent.change(screen.getByRole("searchbox"), { target: { value: "sonnet" } });
    expect(within(list).getAllByRole("option").map((o) => o.getAttribute("data-model-id"))).toEqual(["claude-sonnet-5-5"]);
    fireEvent.change(screen.getByRole("searchbox"), { target: { value: "gpt" } });
    expect(screen.getByText("No models match “gpt”.")).toBeTruthy();
  });

  it("chooses with the pointer and the keyboard, never an unsupported model, and offers only that model's effort levels", async () => {
    ipc.agentsModelsList.mockResolvedValue(catalog("anthropic", CLAUDE));
    render(<Harness initial={{ provider: "anthropic", model: "claude-opus-5-5", effort: "max" }} status={providerStatus()} />);
    const list = await screen.findByRole("listbox", { name: "Models" });
    // Unsupported rows cannot be chosen.
    fireEvent.click(within(list).getAllByRole("option")[3]!);
    expect(valueText()).toBe("anthropic|claude-opus-5-5|max");
    // Keyboard: from the search field into the list, down, Enter.
    fireEvent.keyDown(screen.getByRole("searchbox"), { key: "ArrowDown" });
    expect(document.activeElement?.getAttribute("data-model-id")).toBe("claude-opus-5-5");
    fireEvent.keyDown(document.activeElement!, { key: "ArrowDown" });
    expect(document.activeElement?.getAttribute("data-model-id")).toBe("claude-sonnet-5-5");
    fireEvent.keyDown(document.activeElement!, { key: "End" });
    // End stops at the last model that can be chosen.
    expect(document.activeElement?.getAttribute("data-model-id")).toBe("claude-3-haiku");
    fireEvent.keyDown(document.activeElement!, { key: "Home" });
    fireEvent.keyDown(document.activeElement!, { key: "ArrowDown" });
    fireEvent.keyDown(document.activeElement!, { key: "Enter" });
    expect(valueText()).toBe("anthropic|claude-sonnet-5-5|max");
    expect(within(screen.getByTestId("model-effort")).getByRole("button").textContent).toContain("Max");
    // A model without an effort control says so instead of offering levels.
    fireEvent.click(within(list).getAllByRole("option")[2]!);
    expect(screen.getByTestId("model-no-effort").textContent).toBe("Claude Haiku 3 has no effort setting.");
    expect(screen.queryByTestId("model-effort")).toBeNull();
  });

  it("asks for a visible confirmation before an unverified model can be used", async () => {
    ipc.agentsModelsList.mockResolvedValue(catalog("anthropic", CLAUDE));
    const problems: (string | null)[] = [];
    render(
      <Harness
        initial={{ provider: "anthropic", model: "claude-opus-5-5", effort: "medium" }}
        status={providerStatus()}
        onProblem={(p) => problems.push(p)}
      />,
    );
    const list = await screen.findByRole("listbox", { name: "Models" });
    fireEvent.click(within(list).getAllByRole("option")[2]!);
    const banner = screen.getByTestId("model-unverified");
    expect(banner.textContent).toContain("Older Claude model: not verified.");
    expect(problems.at(-1)).toBe("Confirm that you want to use this unverified model.");
    fireEvent.click(within(banner).getByRole("checkbox", { name: "Use this unverified model" }));
    expect(problems.at(-1)).toBeNull();
  });

  it("keeps a saved model the catalog does not list, marked and selected, never replaced", async () => {
    ipc.agentsModelsList.mockResolvedValue(
      catalog("anthropic", CLAUDE, { suggested: "claude-opus-5-5", suggestedReason: "The newest model Exposition supports for agents on this connection." }),
    );
    const problems: (string | null)[] = [];
    render(<Harness initial={{ provider: "anthropic", model: "claude-legacy-7", effort: "high" }} status={providerStatus()} onProblem={(p) => problems.push(p)} />);
    const list = await screen.findByRole("listbox", { name: "Models" });
    const first = within(list).getAllByRole("option")[0]!;
    expect(first.getAttribute("data-model-id")).toBe("claude-legacy-7");
    expect(first.getAttribute("aria-selected")).toBe("true");
    expect(first.textContent).toContain("Saved, not listed");
    expect(first.textContent).toContain("Not listed by this connection. Exposition checks it when a task starts.");
    expect(valueText()).toBe("anthropic|claude-legacy-7|high");
    expect(problems.at(-1)).toBeNull();
    // The suggestion is offered, with its reason, but not applied.
    expect(within(list).getAllByRole("option")[1]!.textContent).toContain("Suggested");
  });

  it("shows stale and unavailable lists with the reason and fetched time, and refreshes on request", async () => {
    const fetched = Date.now() - 5 * 60_000;
    ipc.agentsModelsList.mockResolvedValueOnce(catalog("anthropic", CLAUDE, { state: "stale", fetchedAtMs: fetched, error: "Could not reach Anthropic to list models." }));
    render(<Harness initial={{ provider: "anthropic", model: "claude-opus-5-5", effort: "medium" }} status={providerStatus()} />);
    const stale = await screen.findByTestId("model-stale");
    expect(stale.textContent).toContain("The model list may be out of date");
    expect(stale.textContent).toContain("Fetched 5 min ago. Could not reach Anthropic to list models.");
    expect(screen.getAllByRole("option")).toHaveLength(4);

    ipc.agentsModelsList.mockResolvedValueOnce(
      catalog("anthropic", [model("claude-opus-5-5", { source: "saved", lifecycle: { kind: "unknown" } })], {
        state: "unavailable",
        fetchedAtMs: null,
        error: "The provider rejected the API key (invalid x-api-key).",
      }),
    );
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Refresh models" }));
    });
    expect(ipc.agentsModelsList).toHaveBeenLastCalledWith("anthropic", true);
    const unavailable = await screen.findByTestId("model-unavailable");
    expect(unavailable.textContent).toContain("The provider rejected the API key (invalid x-api-key). Your chosen model stays selected.");
    expect(screen.getByRole("option").textContent).toContain("Saved, not listed");
    expect(valueText()).toBe("anthropic|claude-opus-5-5|medium");
  });

  it("shows an empty list and a failed request as their own states", async () => {
    ipc.agentsModelsList.mockResolvedValueOnce(catalog("openai", []));
    const { unmount } = render(<Harness initial={{ provider: "openai", model: "", effort: "medium" }} status={providerStatus()} />);
    await screen.findByText("This connection lists no models.");
    unmount();
    ipc.agentsModelsList.mockRejectedValueOnce(new Error("IPC unavailable"));
    render(<Harness initial={{ provider: "openai", model: "gpt-6.2-sol", effort: "medium" }} status={providerStatus()} />);
    await screen.findByText("IPC unavailable");
    expect(screen.getByText("The model list could not be loaded")).toBeTruthy();
    ipc.agentsModelsList.mockResolvedValueOnce(catalog("openai", [model("gpt-6.2-sol")]));
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    });
    expect(await screen.findByRole("option", { name: "gpt-6.2-sol" })).toBeTruthy();
  });

  it("asks for a key instead of listing a connection that has none", () => {
    const status = providerStatus({
      connections: [
        { provider: "anthropic", auth: "apiKey", configured: true, credentialError: null, context: null, version: 1 },
        { provider: "openai", auth: "apiKey", configured: false, credentialError: null, context: null, version: 1 },
      ],
    });
    const configure = vi.fn();
    render(<ModelPicker value={{ provider: "openai", model: "gpt-6.2-sol", effort: "high" }} onChange={() => {}} status={status} onConfigure={configure} />);
    expect(screen.getByText("Connect OpenAI to list its models")).toBeTruthy();
    expect(screen.getByText("gpt-6.2-sol")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Add API key…" }));
    expect(configure).toHaveBeenCalledWith("openai");
    expect(ipc.agentsModelsList).not.toHaveBeenCalled();
  });

  it("discards a late catalog of the previous provider or connection", async () => {
    const anthropic = deferred<ModelCatalog>();
    const openai = deferred<ModelCatalog>();
    ipc.agentsModelsList.mockImplementation((provider: ProviderKind) => (provider === "anthropic" ? anthropic.promise : openai.promise));
    const status = providerStatus();
    const { rerender } = render(<ModelPicker value={{ provider: "anthropic", model: "claude-opus-5-5", effort: "medium" }} onChange={() => {}} status={status} />);
    rerender(<ModelPicker value={{ provider: "openai", model: "gpt-6.2-sol", effort: "medium" }} onChange={() => {}} status={status} />);
    await act(async () => anthropic.resolve(catalog("anthropic", CLAUDE)));
    expect(screen.queryByRole("option", { name: "Claude Opus 5.5" })).toBeNull();
    expect(screen.getByText("Loading models…")).toBeTruthy();
    await act(async () => openai.resolve(catalog("openai", [model("gpt-6.2-sol", { displayName: "GPT-6.2 Sol" })])));
    expect(screen.getByRole("option", { name: "GPT-6.2 Sol" })).toBeTruthy();

    // The late answer arriving after the current one does not replace it either.
    const slow = deferred<ModelCatalog>();
    const fast = deferred<ModelCatalog>();
    ipc.agentsModelsList.mockReset().mockReturnValueOnce(slow.promise).mockReturnValueOnce(fast.promise);
    rerender(<ModelPicker value={{ provider: "anthropic", model: "claude-opus-5-5", effort: "medium" }} onChange={() => {}} status={status} />);
    rerender(<ModelPicker value={{ provider: "openai", model: "gpt-6.2-sol", effort: "medium" }} onChange={() => {}} status={status} />);
    await act(async () => fast.resolve(catalog("openai", [model("gpt-6.2-sol", { displayName: "GPT-6.2 Sol" })])));
    await act(async () => slow.resolve(catalog("anthropic", CLAUDE)));
    expect(screen.getByRole("option", { name: "GPT-6.2 Sol" })).toBeTruthy();
    expect(screen.queryByText("Loading models…")).toBeNull();

    // The same provider on another connection (project changed): the old answer is dropped.
    const before = deferred<ModelCatalog>();
    const after = deferred<ModelCatalog>();
    ipc.agentsModelsList.mockReset().mockReturnValueOnce(before.promise).mockReturnValueOnce(after.promise);
    const other = providerStatus({
      connections: [
        { provider: "anthropic", auth: "apiKey", configured: true, credentialError: null, context: null, version: 1 },
        { provider: "openai", auth: "apiKey", configured: true, credentialError: null, context: "Project proj_b", version: 1 },
      ],
    });
    rerender(<ModelPicker value={{ provider: "openai", model: "gpt-6.2-sol", effort: "medium" }} onChange={() => {}} status={other} revision={1} />);
    rerender(<ModelPicker value={{ provider: "openai", model: "gpt-6.2-sol", effort: "medium" }} onChange={() => {}} status={other} revision={2} />);
    await act(async () => before.resolve(catalog("openai", [model("gpt-old", { displayName: "Old project model" })])));
    expect(screen.queryByRole("option", { name: "Old project model" })).toBeNull();
    await act(async () => after.resolve(catalog("openai", [model("gpt-6.2-sol", { displayName: "GPT-6.2 Sol" })])));
    expect(screen.getByRole("option", { name: "GPT-6.2 Sol" })).toBeTruthy();
  });

  it("switching provider takes that connection's suggestion and restores an earlier choice when switching back", async () => {
    ipc.agentsModelsList.mockImplementation((provider: ProviderKind) =>
      Promise.resolve(
        provider === "anthropic"
          ? catalog("anthropic", CLAUDE)
          : catalog("openai", [model("gpt-6.2-sol", { capabilities: { ...model("x").capabilities, efforts: ["low", "medium", "high"] } }), model("gpt-6.2-luna")], {
              suggested: "gpt-6.2-sol",
              suggestedReason: "The newest model Exposition supports for agents on this connection.",
            }),
      ),
    );
    render(<Harness initial={{ provider: "anthropic", model: "claude-opus-5-5", effort: "max" }} status={providerStatus()} />);
    await screen.findByRole("listbox", { name: "Models" });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: /Provider/ }));
    });
    await act(async () => {
      fireEvent.click(await screen.findByRole("option", { name: "OpenAI" }));
    });
    await waitFor(() => expect(valueText()).toBe("openai|gpt-6.2-sol|medium"));
    expect(screen.getByTestId("model-suggested").textContent).toContain("The newest model Exposition supports");
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: /Provider/ }));
    });
    await act(async () => {
      fireEvent.click(await screen.findByRole("option", { name: "Anthropic" }));
    });
    expect(valueText()).toBe("anthropic|claude-opus-5-5|max");
  });

  it("accepts a custom model ID for adapters that take one, and refuses malformed IDs", async () => {
    ipc.agentsModelsList.mockResolvedValue(catalog("openai", [model("gpt-6.2-sol")]));
    render(<Harness initial={{ provider: "openai", model: "gpt-6.2-sol", effort: "medium" }} status={providerStatus()} />);
    await screen.findByRole("listbox", { name: "Models" });
    fireEvent.click(screen.getByRole("button", { name: "Custom model ID" }));
    const field = screen.getByRole("textbox", { name: "Exact model ID" });
    fireEvent.change(field, { target: { value: "bad id with spaces" } });
    fireEvent.click(screen.getByRole("button", { name: "Use this ID" }));
    expect(screen.getByText(/letters, digits/)).toBeTruthy();
    fireEvent.change(field, { target: { value: "ft:gpt-6.2-sol:acme:42" } });
    fireEvent.click(screen.getByRole("button", { name: "Use this ID" }));
    expect(valueText()).toBe("openai|ft:gpt-6.2-sol:acme:42|medium");
    const row = screen.getAllByRole("option")[0]!;
    expect(row.getAttribute("data-model-id")).toBe("ft:gpt-6.2-sol:acme:42");
    expect(row.textContent).toContain("Custom ID, not listed");
  });

  it("keeps long model names and IDs inside the row (they wrap)", async () => {
    const long = `gpt-6.2-sol-${"very-long-fine-tuned-variant-".repeat(4)}2026-11-17`;
    ipc.agentsModelsList.mockResolvedValue(catalog("openai", [model(long, { displayName: `A ${"remarkably ".repeat(8)}long display name` })]));
    render(<Harness initial={{ provider: "openai", model: long, effort: "medium" }} status={providerStatus()} />);
    const option = await screen.findByRole("option");
    expect(option.getAttribute("data-model-id")).toBe(long);
    expect(option.querySelector(".ui-list-row__title")?.textContent).toContain("remarkably");
    expect(option.querySelector("code")?.textContent).toBe(long);
  });
});
