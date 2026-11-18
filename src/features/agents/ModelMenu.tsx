// The one model menu: every provider's models in one searchable list, grouped
// under the provider's name, then the chosen model's effort and the footer
// actions. Inline content only: the caller supplies the popover or sheet.
// Support facts and lists come from the backend's catalogs (`models.ts`); a
// chosen model a catalog does not list stays chosen and is marked, never
// replaced. Choosing changes the caller's draft only; nothing is requested
// except the model lists themselves.
import { Settings as SettingsIcon } from "lucide-react";
import { useId, useRef, useState, type JSX, type KeyboardEvent, type ReactNode } from "react";

import type { Effort } from "../../bindings/Effort";
import type { ModelDescriptor } from "../../bindings/ModelDescriptor";
import type { ProviderChoice } from "../../bindings/ProviderChoice";
import type { ProviderKind } from "../../bindings/ProviderKind";
import type { ProviderStatus } from "../../bindings/ProviderStatus";
import {
  Badge,
  Banner,
  Button,
  Checkbox,
  ListRow,
  SearchField,
  Select,
  Slider,
  Spinner,
  StatusText,
  TextField,
} from "../../ui/components";
import { EFFORT_LABELS, relativeTime } from "./format";
import {
  catalogRows,
  capabilityHints,
  effortForModel,
  effortLevels,
  filterModels,
  isPlaceholder,
  KEYED_PROVIDERS,
  modelBadges,
  modelNote,
  PROVIDER_NAMES,
  rememberEffort,
  unlistedDescriptor,
  useConnectionCatalog,
  type CatalogRows,
  type ConnectionState,
} from "./models";
import "./modelMenu.css";

export interface ModelMenuProps {
  /** The draft's explicit choice; `null` = the default for new tasks. */
  value: ProviderChoice | null;
  onChange: (choice: ProviderChoice | null) => void;
  status: ProviderStatus | null;
  /** The running turn's model, shown apart from the choice for the next message. */
  running?: { provider: ProviderKind; model: string; effort: Effort | null } | null;
  /** Opens Settings → Model providers. */
  onManageProviders: () => void;
  autoFocus?: boolean;
}

const MODEL_ID = /^[A-Za-z0-9._:@/-]{1,100}$/;

const choiceKey = (c: { provider: ProviderKind; model: string }) => `${c.provider}|${c.model}`;

/** The saved default for new tasks. */
export function defaultChoice(status: ProviderStatus | null): ProviderChoice | null {
  if (!status) return null;
  const { provider, model, effort } = status.settings;
  return { provider, model, effort };
}

/**
 * "Claude Opus 5.5 · High": the model's friendly name when a catalog lists
 * it, else its exact ID; the effort only when the model has one (or it is
 * not known). The scripted provider has no model.
 */
export function choiceSummary(choice: ProviderChoice, descriptor: ModelDescriptor | null): string {
  if (choice.provider === "scripted") return PROVIDER_NAMES.scripted;
  const name = descriptor && !isPlaceholder(descriptor) ? descriptor.displayName : choice.model || "No model chosen";
  const { known, efforts } = effortLevels(descriptor);
  return known && efforts.length === 0 ? name : `${name} · ${EFFORT_LABELS[choice.effort]}`;
}

/** The scripted provider's one entry (development builds). */
function scriptedDescriptor(): ModelDescriptor {
  return {
    provider: "scripted",
    id: "scripted",
    displayName: "Scripted replay",
    lifecycle: { kind: "unknown" },
    agent: { kind: "supported" },
    capabilities: { tools: "unknown", images: "unknown", pdf: "unknown", efforts: [], contextTokens: null, maxOutputTokens: null },
    source: "catalog",
    createdAtMs: null,
  };
}

function ModelOption({
  model,
  selected,
  focusable,
  suggested,
  custom,
  onSelect,
}: {
  model: ModelDescriptor;
  selected: boolean;
  focusable: boolean;
  suggested: string | null;
  custom: boolean;
  onSelect: () => void;
}) {
  const metaId = useId();
  const disabled = model.agent.kind === "unsupported";
  const hints = model.provider === "scripted" ? ["Replays development scripts"] : capabilityHints(model);
  const note = model.provider === "scripted" ? null : modelNote(model);
  const badges = modelBadges(model, suggested, custom);
  return (
    <ListRow
      role="option"
      aria-selected={selected}
      aria-disabled={disabled || undefined}
      aria-label={model.displayName}
      aria-describedby={metaId}
      tabIndex={focusable ? 0 : -1}
      selected={selected}
      className="agents-model-option"
      data-provider={model.provider}
      data-model-id={model.id}
      data-testid="model-option"
      onClick={() => {
        if (!disabled) onSelect();
      }}
      title={model.displayName}
      meta={
        <span id={metaId} className="agents-model-option__meta">
          {model.provider !== "scripted" && <code className="ui-code">{model.id}</code>}
          {hints.length > 0 && <span>{hints.join(" · ")}</span>}
          {note && <span className="agents-model-option__note">{note}</span>}
        </span>
      }
      trailing={
        badges.length > 0 ? (
          <span className="agents-model-option__badges">
            {badges.map((b) => (
              <Badge key={b.label} tone={b.tone} dashed={b.dashed}>
                {b.label}
              </Badge>
            ))}
          </span>
        ) : undefined
      }
    />
  );
}

/** A group's list state beside its heading: listed / stale / loading / error / not connected. */
function GroupState({
  provider,
  connection,
  state,
  fetchedAtMs,
  loading,
  failed,
  onRefresh,
  onManageProviders,
}: {
  provider: ProviderKind;
  connection: ConnectionState;
  state: "fresh" | "stale" | "unavailable" | null;
  fetchedAtMs: number | null;
  loading: boolean;
  failed: boolean;
  onRefresh: () => void;
  onManageProviders: () => void;
}) {
  const name = PROVIDER_NAMES[provider];
  if (!connection.configured) {
    return (
      <>
        <StatusText status={connection.credentialError ? "failed" : "unavailable"} label={connection.credentialError ? "Credential storage error" : "Not connected"} />
        <Button size="sm" variant="ghost" icon={SettingsIcon} onPress={onManageProviders} data-testid="model-configure">
          Configure in Settings
        </Button>
      </>
    );
  }
  if (loading && state === null && !failed) {
    return (
      <span className="agents-model-group__loading">
        <Spinner size="sm" aria-hidden />
        <span className="ui-meta">Loading models…</span>
      </span>
    );
  }
  const fetched = fetchedAtMs ? relativeTime(fetchedAtMs) : null;
  if (failed || state === "unavailable") {
    return (
      <>
        <StatusText status="failed" label={failed ? "Could not load the list" : `${name} did not list its models`} />
        <Button size="sm" variant="ghost" isPending={loading} onPress={onRefresh} aria-label={`Retry listing ${name} models`}>
          Retry
        </Button>
      </>
    );
  }
  if (state === "stale") {
    return (
      <>
        <StatusText status="stale" label={fetched ? `Listed ${fetched}; refresh failed` : "Refresh failed"} />
        <Button size="sm" variant="ghost" isPending={loading} onPress={onRefresh} aria-label={`Refresh ${name} models`}>
          Refresh
        </Button>
      </>
    );
  }
  return <span className="ui-meta">{fetched ? `Listed ${fetched}` : "Listed"}</span>;
}

/** Focus the next enabled option in `direction` (or the first/last) across every group. */
function moveFocus(root: HTMLElement, direction: 1 | -1 | "first" | "last", search: HTMLInputElement | null) {
  const options = [...root.querySelectorAll<HTMLElement>('[role="option"]:not([aria-disabled="true"])')];
  if (!options.length) return;
  const index = options.indexOf(document.activeElement as HTMLElement);
  if (direction === -1 && index <= 0) {
    search?.focus();
    return;
  }
  const next =
    direction === "first"
      ? options[0]
      : direction === "last"
        ? options[options.length - 1]
        : options[Math.min(options.length - 1, Math.max(0, index < 0 ? 0 : index + direction))];
  next?.focus();
}

interface Group {
  provider: ProviderKind;
  connection: ConnectionState;
  rows: CatalogRows;
  visible: ModelDescriptor[];
  load: ReturnType<typeof useConnectionCatalog> | null;
  /** The list is not shown (loading, failed, or not connected). */
  listless: boolean;
}

export interface ModelMenuContentProps extends ModelMenuProps {
  /** Show "Use default (…)" and "Manage providers…" (off where the default itself is edited). */
  footer?: boolean;
  testId?: string;
}

/**
 * The menu with the options Settings needs (no footer when the default
 * itself is being edited). Feature code uses `ModelMenu`.
 */
export function ModelMenuContent({
  value,
  onChange,
  status,
  running,
  onManageProviders,
  autoFocus,
  footer = true,
  testId,
}: ModelMenuContentProps): JSX.Element {
  const fallback = defaultChoice(status);
  const effective = value ?? fallback;
  // The choice the menu opened with counts as acknowledged (it is the
  // person's existing explicit choice); any other unverified model needs a
  // visible confirmation before it reaches the draft.
  const [acknowledged, setAcknowledged] = useState(() => new Set(effective ? [choiceKey(effective)] : []));
  const [pending, setPending] = useState<ProviderChoice | null>(null);
  const [custom, setCustom] = useState<Set<string>>(() => new Set());
  const [query, setQuery] = useState("");
  const [customOpen, setCustomOpen] = useState(false);
  const [customProvider, setCustomProvider] = useState<ProviderKind>(effective?.provider === "openai" ? "openai" : "anthropic");
  const [customId, setCustomId] = useState("");
  const [customError, setCustomError] = useState<string | null>(null);
  const groupsRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLDivElement>(null);

  // One catalog per keyed provider (hooks in a fixed order).
  const anthropic = useConnectionCatalog(status, "anthropic");
  const openai = useConnectionCatalog(status, "openai");
  const loads: Record<"anthropic" | "openai", typeof anthropic> = { anthropic, openai };

  const shown = pending ?? effective;
  const groups: Group[] = KEYED_PROVIDERS.map((provider) => {
    const load = loads[provider as "anthropic" | "openai"];
    const chosen: ProviderChoice = shown && shown.provider === provider ? shown : { provider, model: "", effort: "medium" };
    const listless = !load.connection.configured ? false : load.catalog === null;
    const rows = listless ? { rows: [], selected: null, unlisted: false } : catalogRows(load.catalog, chosen);
    return { provider, connection: load.connection, rows, visible: filterModels(rows.rows, query), load, listless };
  });
  if (status?.scriptedAvailable) {
    const scripted = scriptedDescriptor();
    groups.push({
      provider: "scripted",
      connection: { provider: "scripted", configured: true, credentialError: null, context: null, version: 0 },
      rows: { rows: [scripted], selected: shown?.provider === "scripted" ? scripted : null, unlisted: false },
      visible: filterModels([scripted], query),
      load: null,
      listless: false,
    });
  }

  const isSelected = (m: ModelDescriptor) =>
    shown !== null && shown.provider === m.provider && (m.provider === "scripted" || shown.model === m.id);
  const options = groups.flatMap((g) => g.visible);
  const focusTarget = options.find((m) => isSelected(m) && m.agent.kind !== "unsupported") ?? options.find((m) => m.agent.kind !== "unsupported");

  // The effective choice's descriptor (`null` while its list is not loaded).
  const effectiveGroup = effective ? groups.find((g) => g.provider === effective.provider) : undefined;
  const effectiveDescriptor =
    effective && effectiveGroup && !effectiveGroup.listless
      ? effectiveGroup.provider === "scripted"
        ? effectiveGroup.rows.selected
        : catalogRows(effectiveGroup.load?.catalog ?? null, effective).selected
      : null;
  const pendingDescriptor = pending ? (groups.find((g) => g.provider === pending.provider)?.rows.selected ?? null) : null;
  const fallbackGroup = fallback ? groups.find((g) => g.provider === fallback.provider) : undefined;
  const fallbackDescriptor =
    fallback && fallbackGroup && !fallbackGroup.listless
      ? fallback.provider === "scripted"
        ? null
        : catalogRows(fallbackGroup.load?.catalog ?? null, fallback).selected
      : null;

  const emit = (next: ProviderChoice | null) => {
    if (next) rememberEffort(next);
    onChange(next);
  };

  const select = (m: ModelDescriptor) => {
    if (m.agent.kind === "unsupported") return;
    if (effective) rememberEffort(effective);
    const current = effective?.effort ?? "medium";
    const next: ProviderChoice =
      m.provider === "scripted"
        ? { provider: "scripted", model: effective?.provider === "scripted" ? effective.model : "scripted", effort: current }
        : { provider: m.provider, model: m.id, effort: effortForModel(m, current) };
    if (m.agent.kind === "unverified" && !acknowledged.has(choiceKey(next))) {
      setPending(next);
      return;
    }
    setPending(null);
    emit(next);
  };

  const applyCustom = () => {
    const id = customId.trim();
    if (!MODEL_ID.test(id)) {
      setCustomError("Use the provider's exact model ID: letters, digits, and . - _ : @ / only.");
      return;
    }
    setCustomError(null);
    setCustomId("");
    setCustomOpen(false);
    const group = groups.find((g) => g.provider === customProvider);
    const listed = group?.load?.catalog?.models.find((m) => m.id === id);
    if (listed) {
      select(listed);
      return;
    }
    // Typing an exact ID is an explicit choice.
    setCustom((s) => new Set(s).add(choiceKey({ provider: customProvider, model: id })));
    setAcknowledged((s) => new Set(s).add(choiceKey({ provider: customProvider, model: id })));
    select(unlistedDescriptor(customProvider, id));
  };

  const onListKey = (event: KeyboardEvent<HTMLDivElement>) => {
    const root = groupsRef.current;
    const target = (event.target as HTMLElement).closest<HTMLElement>('[role="option"]');
    if (!root || !target) return;
    const search = searchRef.current?.querySelector("input") ?? null;
    switch (event.key) {
      case "ArrowDown":
        moveFocus(root, 1, search);
        break;
      case "ArrowUp":
        moveFocus(root, -1, search);
        break;
      case "Home":
        moveFocus(root, "first", search);
        break;
      case "End":
        moveFocus(root, "last", search);
        break;
      case "Enter":
      case " ": {
        const model = options.find((m) => m.provider === target.dataset.provider && m.id === target.dataset.modelId);
        if (model) select(model);
        break;
      }
      default:
        return;
    }
    event.preventDefault();
  };

  const anyMatch = groups.some((g) => g.visible.length > 0);
  const suggestedOf = (g: Group) => g.load?.catalog?.suggested ?? null;

  let effort: ReactNode = null;
  if (effective) {
    const name = PROVIDER_NAMES[effective.provider];
    if (effective.provider === "scripted") {
      effort = (
        <p className="ui-meta" data-testid="model-no-effort">
          Effort is managed by this model.
        </p>
      );
    } else if (!effectiveDescriptor && effectiveGroup?.connection.configured && effectiveGroup.load?.loading) {
      effort = <p className="ui-meta">Loading effort levels…</p>;
    } else {
      const { known, efforts } = effortLevels(effectiveDescriptor);
      effort =
        efforts.length === 0 ? (
          <p className="ui-meta" data-testid="model-no-effort">
            Effort is managed by this model.
          </p>
        ) : (
          <Slider<Effort>
            label="Effort"
            values={efforts.map((id) => ({ id, label: EFFORT_LABELS[id] }))}
            value={efforts.includes(effective.effort) ? effective.effort : null}
            onChange={(next) => emit({ ...effective, effort: next })}
            description={
              known
                ? `Levels are specific to ${name}: the same name can behave differently with another provider.`
                : `Exposition does not know which levels ${effective.model} accepts. Levels are specific to ${name}.`
            }
            className="agents-model-menu__slider"
          />
        );
    }
  }

  const runningGroup = running ? groups.find((g) => g.provider === running.provider) : undefined;
  const runningName =
    running && (runningGroup?.load?.catalog?.models.find((m) => m.id === running.model)?.displayName ?? (running.provider === "scripted" ? PROVIDER_NAMES.scripted : running.model));

  return (
    <div className="agents-model-menu" data-testid={testId ?? "model-menu"}>
      {running && (
        <div className="agents-model-menu__running" data-testid="model-running">
          <span className="ui-section-title">For the next message</span>
          <span className="ui-meta">
            Running: {[runningName, running.effort ? EFFORT_LABELS[running.effort] : null].filter(Boolean).join(" · ")}
          </span>
        </div>
      )}
      <div ref={searchRef} className="agents-model-menu__search">
        <SearchField
          aria-label="Search models"
          placeholder="Search models by name or ID"
          value={query}
          onChange={setQuery}
          autoFocus={autoFocus}
          onKeyDown={(event) => {
            if (event.key === "ArrowDown" && groupsRef.current) {
              event.preventDefault();
              const target =
                groupsRef.current.querySelector<HTMLElement>('[role="option"][tabindex="0"]') ??
                groupsRef.current.querySelector<HTMLElement>('[role="option"]:not([aria-disabled="true"])');
              target?.focus();
              return;
            }
            // Escape and Tab reach the popover or sheet that holds the menu.
            event.continuePropagation();
          }}
          data-testid="model-search"
        />
      </div>
      <div ref={groupsRef} className="agents-model-menu__groups" onKeyDown={onListKey} data-testid="model-groups">
        {groups.map((g) => {
          const name = PROVIDER_NAMES[g.provider];
          const keyed = g.provider !== "scripted";
          // While searching, groups without matches step aside (an unconnected one keeps its link).
          if (query.trim() && g.visible.length === 0 && g.connection.configured) return null;
          const catalog = g.load?.catalog ?? null;
          return (
            <section key={g.provider} className="agents-model-group" aria-label={name} data-testid="model-group" data-provider={g.provider}>
              <div className="agents-model-group__header">
                <span className="ui-group-label">{name}</span>
                {keyed && g.load && (
                  <span className="agents-model-group__state" data-testid="model-group-state">
                    <GroupState
                      provider={g.provider}
                      connection={g.connection}
                      state={catalog?.state ?? null}
                      fetchedAtMs={catalog?.fetchedAtMs ?? null}
                      loading={g.load.loading}
                      failed={g.load.error !== null}
                      onRefresh={g.load.refresh}
                      onManageProviders={onManageProviders}
                    />
                  </span>
                )}
              </div>
              {g.load?.error && (
                <p className="ui-meta agents-model-group__note" data-testid="model-group-error">
                  {g.load.error}
                </p>
              )}
              {catalog && (catalog.state === "stale" || catalog.state === "unavailable") && catalog.error && (
                <p className="ui-meta agents-model-group__note">
                  {catalog.error}
                  {catalog.state === "unavailable" ? " Your chosen model stays selected." : ""}
                </p>
              )}
              {keyed && g.connection.configured && catalog && g.rows.rows.length === 0 && (
                <p className="ui-meta agents-model-group__note">This connection lists no models.</p>
              )}
              {g.visible.length > 0 && (
                <div role="listbox" aria-label={`${name} models`} className="agents-model-group__list">
                  {g.visible.map((m) => (
                    <ModelOption
                      key={m.id}
                      model={m}
                      selected={isSelected(m)}
                      focusable={focusTarget === m}
                      suggested={suggestedOf(g)}
                      custom={custom.has(`${m.provider}|${m.id}`)}
                      onSelect={() => select(m)}
                    />
                  ))}
                </div>
              )}
            </section>
          );
        })}
        {!anyMatch && query.trim() && <p className="ui-meta agents-model-group__note">No models match “{query.trim()}”.</p>}
      </div>
      {pending && pendingDescriptor && pendingDescriptor.agent.kind === "unverified" && (
        <Banner tone="warning" title="Not verified for agents" data-testid="model-unverified" className="agents-model-menu__notice">
          <p>{pendingDescriptor.agent.reason} Tool use or attachments may fail with it.</p>
          <Checkbox
            isSelected={false}
            onChange={(on) => {
              if (!on) return;
              setAcknowledged((s) => new Set(s).add(choiceKey(pending)));
              setPending(null);
              emit(pending);
            }}
            data-testid="model-acknowledge"
          >
            Use this unverified model
          </Checkbox>
        </Banner>
      )}
      {!pending && effectiveDescriptor && !isPlaceholder(effectiveDescriptor) && effectiveDescriptor.agent.kind === "unverified" && (
        <p className="ui-meta agents-model-menu__notice" data-testid="model-unverified-chosen">
          {effectiveDescriptor.displayName} is not verified for agents: {effectiveDescriptor.agent.reason}
        </p>
      )}
      {!pending && effectiveDescriptor && effectiveDescriptor.agent.kind === "unsupported" && (
        <Banner tone="danger" title="This model cannot run agents" data-testid="model-unsupported" className="agents-model-menu__notice">
          {effectiveDescriptor.agent.reason} Choose another model.
        </Banner>
      )}
      <div className="agents-model-menu__custom" data-testid="model-custom">
        {customOpen ? (
          <div className="agents-model-menu__custom-form">
            <Select<ProviderKind>
              label="Provider"
              value={customProvider}
              onChange={setCustomProvider}
              options={KEYED_PROVIDERS.map((id) => ({ id, label: PROVIDER_NAMES[id] }))}
            />
            <TextField
              label="Exact model ID"
              code
              value={customId}
              autoFocus
              onChange={(v) => {
                setCustomId(v);
                setCustomError(null);
              }}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  applyCustom();
                  return;
                }
                if (event.key === "Escape") {
                  // The first Escape closes the field; the next one the menu.
                  setCustomOpen(false);
                  return;
                }
                event.continuePropagation();
              }}
              isInvalid={customError !== null}
              errorMessage={customError ?? undefined}
              className="agents-model-menu__custom-id"
              data-testid="model-custom-input"
            />
            <Button size="md" isDisabled={!customId.trim()} onPress={applyCustom} data-testid="model-custom-use">
              Use this ID
            </Button>
            <p className="ui-meta agents-model-menu__custom-note">For a model this connection does not list. Exposition checks it when a task starts.</p>
          </div>
        ) : (
          <Button size="sm" variant="ghost" onPress={() => setCustomOpen(true)} data-testid="model-custom-open">
            Custom model ID…
          </Button>
        )}
      </div>
      {effort && (
        <div className="agents-model-menu__effort" data-testid="model-effort">
          {effort}
        </div>
      )}
      {footer && (
        <div className="agents-model-menu__footer">
          <Button size="sm" variant="ghost" isDisabled={value === null || !fallback} onPress={() => onChange(null)} data-testid="model-use-default">
            {/* The default can name a long model: it truncates in a narrow panel (the accessible name stays whole). */}
            <span className="agents-model-menu__default-label">{`Use default (${fallback ? choiceSummary(fallback, fallbackDescriptor) : "not loaded"})`}</span>
          </Button>
          <Button size="sm" variant="ghost" onPress={onManageProviders} data-testid="model-manage-providers">
            Manage providers…
          </Button>
        </div>
      )}
    </div>
  );
}

/**
 * One searchable model menu grouped by provider, the chosen model's effort,
 * "Use default (…)" and "Manage providers…". Inline content: the caller
 * supplies the popover or sheet. Changes go to `onChange` only (the draft);
 * nothing is sent and the saved default does not change.
 */
export function ModelMenu(props: ModelMenuProps): JSX.Element {
  return <ModelMenuContent {...props} />;
}
