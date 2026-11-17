// Model choice: provider connections, catalog rows, and what a chosen model
// supports (pure; unit tested). The catalog itself comes from the backend
// (`agents_models_list`), which owns discovery and the support rules; this
// module only presents it and never decides support on its own.
import { useCallback, useEffect, useRef, useState } from "react";

import type { Effort } from "../../bindings/Effort";
import type { ModelCatalog } from "../../bindings/ModelCatalog";
import type { ModelDescriptor } from "../../bindings/ModelDescriptor";
import type { ProviderChoice } from "../../bindings/ProviderChoice";
import type { ProviderKind } from "../../bindings/ProviderKind";
import type { ProviderStatus } from "../../bindings/ProviderStatus";
import { commands, errorMessage } from "../../lib/ipc";

export const PROVIDER_NAMES: Record<ProviderKind, string> = {
  anthropic: "Anthropic",
  openai: "OpenAI",
  scripted: "Scripted provider — development only",
};

/** Providers that connect with an API key (the scripted one has none). */
export const KEYED_PROVIDERS: ProviderKind[] = ["anthropic", "openai"];

export interface ConnectionState {
  provider: ProviderKind;
  configured: boolean;
  credentialError: string | null;
  /** Non-secret context, e.g. "Project proj_…". */
  context: string | null;
  /** Changes whenever the connection's key is saved or removed. */
  version: number;
}

/**
 * A provider's connection as the status reports it. Statuses without
 * per-connection entries (older backends, tests) describe the default
 * provider only; any other provider then counts as not configured.
 */
export function connectionOf(status: ProviderStatus | null, provider: ProviderKind): ConnectionState {
  if (provider === "scripted") {
    return { provider, configured: status?.scriptedAvailable ?? false, credentialError: null, context: null, version: 0 };
  }
  const entry = status?.connections?.find((c) => c.provider === provider);
  if (entry) {
    return { provider, configured: entry.configured, credentialError: entry.credentialError, context: entry.context, version: entry.version };
  }
  if (status && status.settings.provider === provider) {
    return { provider, configured: status.configured, credentialError: status.credentialError, context: null, version: 0 };
  }
  return { provider, configured: false, credentialError: null, context: null, version: 0 };
}

/** The providers a picker offers (the scripted one only where it exists). */
export function availableProviders(status: ProviderStatus | null): ProviderKind[] {
  return status?.scriptedAvailable ? [...KEYED_PROVIDERS, "scripted"] : [...KEYED_PROVIDERS];
}

/** "Anthropic · claude-opus-5-5 · Medium effort" (parts joined by the caller). */
export function choiceParts(choice: ProviderChoice, effortLabel: (e: Effort) => string): string[] {
  if (choice.provider === "scripted") return [PROVIDER_NAMES.scripted];
  return [PROVIDER_NAMES[choice.provider], choice.model || "no model chosen", `${effortLabel(choice.effort)} effort`];
}

/** Rows whose display name or exact ID contain every word of the query. */
export function filterModels(models: ModelDescriptor[], query: string): ModelDescriptor[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return models;
  return models.filter((m) => {
    const text = `${m.displayName} ${m.id}`.toLowerCase();
    return words.every((w) => text.includes(w));
  });
}

const placeholders = new WeakSet<ModelDescriptor>();

/**
 * A chosen model the catalog does not list (saved earlier, or typed as a
 * custom ID): kept exactly as itself, never replaced. Exposition claims
 * nothing about what it supports; the backend checks it when a task starts.
 */
export function unlistedDescriptor(provider: ProviderKind, id: string): ModelDescriptor {
  const descriptor: ModelDescriptor = {
    provider,
    id,
    displayName: id,
    lifecycle: { kind: "unknown" },
    agent: { kind: "supported" },
    capabilities: { tools: "unknown", images: "unknown", pdf: "unknown", efforts: [], contextTokens: null, maxOutputTokens: null },
    source: "saved",
    createdAtMs: null,
  };
  placeholders.add(descriptor);
  return descriptor;
}

/** A row made here for an unlisted choice (no support facts from Exposition). */
export function isPlaceholder(m: ModelDescriptor | null): boolean {
  return m !== null && placeholders.has(m);
}

export interface CatalogRows {
  rows: ModelDescriptor[];
  /** The chosen model's descriptor (`null` when nothing is chosen). */
  selected: ModelDescriptor | null;
  /** The chosen model is not in the catalog (saved or typed). */
  unlisted: boolean;
}

/** The rows to show: the catalog plus the chosen model when it is not listed (first). */
export function catalogRows(catalog: ModelCatalog | null, choice: ProviderChoice): CatalogRows {
  const models = catalog && catalog.provider === choice.provider ? catalog.models : [];
  if (!choice.model) return { rows: models, selected: null, unlisted: false };
  const found = models.find((m) => m.id === choice.model);
  if (found) return { rows: models, selected: found, unlisted: found.source === "saved" };
  const saved = unlistedDescriptor(choice.provider, choice.model);
  return { rows: [saved, ...models], selected: saved, unlisted: true };
}

export type BadgeSpec = { label: string; tone: "neutral" | "warning" | "danger" | "accent"; dashed?: boolean };

/** Lifecycle and support badges of a row (`custom`: an ID the user typed). */
export function modelBadges(m: ModelDescriptor, suggested: string | null, custom = false): BadgeSpec[] {
  const badges: BadgeSpec[] = [];
  if (m.source === "saved") badges.push({ label: custom ? "Custom ID, not listed" : "Saved, not listed", tone: "neutral", dashed: true });
  if (m.agent.kind === "unverified") badges.push({ label: "Unverified", tone: "warning", dashed: true });
  if (m.agent.kind === "unsupported") badges.push({ label: "Unavailable", tone: "neutral", dashed: true });
  if (m.lifecycle.kind === "deprecated") badges.push({ label: "Deprecated", tone: "warning" });
  if (m.lifecycle.kind === "retired") badges.push({ label: "Retired", tone: "danger" });
  if (suggested === m.id && m.source === "catalog") badges.push({ label: "Suggested", tone: "accent", dashed: true });
  return badges;
}

/** What the model accepts, as short related facts ("Images · PDF · Effort"). Unknown is not claimed. */
export function capabilityHints(m: ModelDescriptor): string[] {
  const hints: string[] = [];
  if (m.capabilities.images === "yes") hints.push("Images");
  if (m.capabilities.pdf === "yes") hints.push("PDF");
  if (m.capabilities.efforts.length) hints.push("Effort");
  if (m.capabilities.contextTokens) hints.push(`${compactTokens(m.capabilities.contextTokens)} context`);
  return hints;
}

function compactTokens(n: number): string {
  if (n >= 1_000_000) return `${+(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1000) return `${Math.round(n / 1000)}k`;
  return String(n);
}

/** A row's explanatory line: why it is unverified or unavailable, when it shuts down. */
export function modelNote(m: ModelDescriptor): string | null {
  if (isPlaceholder(m)) return "Not listed by this connection. Exposition checks it when a task starts.";
  if (m.agent.kind === "unsupported") return m.agent.reason;
  if (m.agent.kind === "unverified") return m.agent.reason;
  if (m.lifecycle.kind === "deprecated") return m.lifecycle.date ? `Shuts down ${m.lifecycle.date}.` : "Scheduled to shut down.";
  return null;
}

/**
 * The effort to keep after choosing a model: the current one when the model
 * accepts it, else Medium when offered, else the model's first level. Models
 * without an effort control keep the value (it is not sent for them).
 */
export function effortFor(current: Effort, efforts: Effort[]): Effort {
  if (!efforts.length || efforts.includes(current)) return current;
  if (efforts.includes("medium")) return "medium";
  return efforts[0]!;
}

/** Why the choice cannot be used yet (`null`: it can). */
export function choiceProblem(
  choice: ProviderChoice,
  selected: ModelDescriptor | null,
  acknowledged: boolean,
): string | null {
  if (choice.provider === "scripted") return null;
  if (!choice.model.trim()) return "Choose a model.";
  if (!selected) return null;
  if (selected.agent.kind === "unsupported") return `${selected.id} cannot run agents: ${selected.agent.reason}`;
  if (selected.agent.kind === "unverified" && !acknowledged) return "Confirm that you want to use this unverified model.";
  return null;
}

// ---------------------------------------------------------------- catalog loading

export interface CatalogLoad {
  catalog: ModelCatalog | null;
  loading: boolean;
  /** The request itself failed (the catalog's own state covers provider errors). */
  error: string | null;
  refresh: () => void;
}

/**
 * The catalog of one connection. A new provider or connection identity
 * clears what was shown and loads again; a response that arrives after the
 * provider or connection changed is discarded (request sequencing), so an
 * old connection's models never replace the current ones.
 */
export function useModelCatalog(provider: ProviderKind | null, identity: string, enabled: boolean): CatalogLoad {
  const key = enabled && provider ? `${provider}|${identity}` : null;
  const [state, setState] = useState<{ key: string | null; catalog: ModelCatalog | null; loading: boolean; error: string | null }>({
    key: null,
    catalog: null,
    loading: false,
    error: null,
  });
  const sequence = useRef(0);

  const load = useCallback(
    (refresh: boolean) => {
      if (!key || !provider) return;
      const mine = ++sequence.current;
      setState((s) => ({ key, catalog: s.key === key ? s.catalog : null, loading: true, error: null }));
      void (async () => {
        try {
          const catalog = await commands.agentsModelsList(provider, refresh);
          if (mine !== sequence.current || catalog.provider !== provider) return;
          setState({ key, catalog, loading: false, error: null });
        } catch (e) {
          if (mine !== sequence.current) return;
          setState((s) => ({ ...s, key, loading: false, error: errorMessage(e) }));
        }
      })();
    },
    [key, provider],
  );

  useEffect(() => {
    if (!key) {
      // Nothing to load; a response still in flight belongs to another connection.
      sequence.current++;
      return;
    }
    load(false);
  }, [key, load]);

  const current = state.key === key;
  return {
    catalog: current ? state.catalog : null,
    loading: key !== null && (!current || state.loading),
    error: current ? state.error : null,
    refresh: () => load(true),
  };
}
