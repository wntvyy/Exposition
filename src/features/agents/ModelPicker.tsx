// The one model picker: provider/connection → (connect it) → a searchable list
// of the connection's models → the settings that model supports. Used by the
// provider settings (the default for new tasks), the task composer, and the
// follow-up form. Support facts come from the backend's catalog; a chosen model
// the catalog does not list stays chosen and is marked, never replaced.
import { KeyRound, RefreshCw } from "lucide-react";
import { useEffect, useId, useMemo, useRef, useState, type KeyboardEvent } from "react";

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
  EmptyState,
  ErrorState,
  IconButton,
  ListRow,
  LoadingState,
  SearchField,
  Section,
  Select,
  TextField,
  UnavailableState,
} from "../../ui/components";
import { EFFORT_LABELS, relativeTime } from "./format";
import {
  availableProviders,
  capabilityHints,
  catalogRows,
  choiceParts,
  choiceProblem,
  connectionOf,
  effortFor,
  filterModels,
  isPlaceholder,
  modelBadges,
  modelNote,
  PROVIDER_NAMES,
  unlistedDescriptor,
  useModelCatalog,
} from "./models";

const ALL_EFFORTS = Object.keys(EFFORT_LABELS) as Effort[];
const MODEL_ID = /^[A-Za-z0-9._:@/-]{1,100}$/;

export interface ModelPickerProps {
  value: ProviderChoice;
  onChange: (value: ProviderChoice) => void;
  status: ProviderStatus | null;
  /** Why the choice cannot be used yet (`null`: it can), for the owner's submit button. */
  onProblemChange?: (problem: string | null) => void;
  /** Set up a connection that has no API key. */
  onConfigure?: (provider: ProviderKind) => void;
  /** Bump after a credential change the status cannot show (a replaced key) to reload. */
  revision?: number;
  testId?: string;
}

const choiceKey = (c: { provider: ProviderKind; model: string }) => `${c.provider}|${c.model}`;

function ModelRow({
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
  const hints = capabilityHints(model);
  const note = modelNote(model);
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
      data-model-id={model.id}
      data-testid="model-option"
      onClick={() => {
        if (!disabled) onSelect();
      }}
      title={model.displayName}
      meta={
        <span id={metaId} className="agents-model-row__meta">
          <code className="ui-code">{model.id}</code>
          {hints.length > 0 && <span>{hints.join(" · ")}</span>}
          {note && <span className="agents-model-row__note">{note}</span>}
        </span>
      }
      trailing={
        badges.length > 0 ? (
          <span className="agents-badges">
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

/** Focus the next enabled option in `direction` (or the first/last). */
function moveFocus(list: HTMLElement, direction: 1 | -1 | "first" | "last") {
  const options = [...list.querySelectorAll<HTMLElement>('[role="option"]:not([aria-disabled="true"])')];
  if (!options.length) return;
  const index = options.indexOf(document.activeElement as HTMLElement);
  const next =
    direction === "first"
      ? options[0]
      : direction === "last"
        ? options[options.length - 1]
        : options[Math.min(options.length - 1, Math.max(0, index < 0 ? 0 : index + direction))];
  next?.focus();
}

export function ModelPicker({ value, onChange, status, onProblemChange, onConfigure, revision = 0, testId }: ModelPickerProps) {
  const initial = useRef(value);
  // The choice the picker opened with counts as acknowledged: it is the
  // person's existing explicit choice. Any other unverified model needs a
  // visible confirmation first.
  const [acknowledged, setAcknowledged] = useState(() => new Set([choiceKey(initial.current)]));
  const [custom, setCustom] = useState<Set<string>>(() => new Set());
  const remembered = useRef<Partial<Record<ProviderKind, { model: string; effort: Effort }>>>({
    [initial.current.provider]: { model: initial.current.model, effort: initial.current.effort },
  });
  const suggestFor = useRef<ProviderKind | null>(null);
  const [query, setQuery] = useState("");
  const [customId, setCustomId] = useState("");
  const [customError, setCustomError] = useState<string | null>(null);
  const listRef = useRef<HTMLDivElement>(null);

  const provider = value.provider;
  const connection = connectionOf(status, provider);
  const identity = `${connection.configured}|${connection.context ?? ""}|${connection.credentialError ?? ""}|${revision}`;
  const listing = provider !== "scripted" && connection.configured;
  const { catalog, loading, error, refresh } = useModelCatalog(provider, identity, listing);
  const { rows, selected } = catalogRows(catalog, value);
  const visible = useMemo(() => filterModels(rows, query), [rows, query]);
  const placeholder = isPlaceholder(selected);
  const ack = acknowledged.has(choiceKey(value));
  const problem = listing || provider === "scripted" ? choiceProblem(value, selected, ack) : value.model ? null : "Choose a model.";

  useEffect(() => {
    onProblemChange?.(problem);
  }, [problem, onProblemChange]);

  // After switching to a provider with no earlier choice, take the catalog's
  // suggestion (an explainable policy, shown with its reason), once.
  useEffect(() => {
    if (!catalog || suggestFor.current !== provider || catalog.provider !== provider) return;
    suggestFor.current = null;
    if (value.model || !catalog.suggested) return;
    const model = catalog.models.find((m) => m.id === catalog.suggested);
    onChange({ provider, model: catalog.suggested, effort: effortFor(value.effort, model?.capabilities.efforts ?? []) });
  }, [catalog, provider, value, onChange]);

  const choose = (model: ModelDescriptor) => {
    const next = { provider, model: model.id, effort: effortFor(value.effort, model.capabilities.efforts) };
    remembered.current[provider] = { model: next.model, effort: next.effort };
    onChange(next);
  };

  const switchProvider = (next: ProviderKind) => {
    if (next === provider) return;
    remembered.current[provider] = { model: value.model, effort: value.effort };
    setQuery("");
    const earlier = remembered.current[next];
    if (next === "scripted") {
      // The scripted provider ignores the model; keep a valid identifier.
      onChange({ provider: next, model: earlier?.model || value.model || "scripted", effort: value.effort });
      return;
    }
    if (earlier?.model) {
      onChange({ provider: next, model: earlier.model, effort: earlier.effort });
      return;
    }
    suggestFor.current = next;
    onChange({ provider: next, model: "", effort: value.effort });
  };

  const applyCustom = () => {
    const id = customId.trim();
    if (!MODEL_ID.test(id)) {
      setCustomError("Use the provider's exact model ID: letters, digits, and . - _ : @ / only.");
      return;
    }
    setCustomError(null);
    setCustomId("");
    const listed = rows.find((m) => m.id === id && !isPlaceholder(m));
    if (listed) {
      choose(listed);
      return;
    }
    setCustom((s) => new Set(s).add(id));
    // Typing an exact ID is an explicit choice.
    setAcknowledged((s) => new Set(s).add(choiceKey({ provider, model: id })));
    choose(unlistedDescriptor(provider, id));
  };

  const onListKey = (event: KeyboardEvent<HTMLDivElement>) => {
    const list = listRef.current;
    if (!list) return;
    switch (event.key) {
      case "ArrowDown":
        moveFocus(list, 1);
        break;
      case "ArrowUp": {
        const first = list.querySelector<HTMLElement>('[role="option"]:not([aria-disabled="true"])');
        if (document.activeElement === first) list.closest("[data-model-picker]")?.querySelector<HTMLInputElement>("input[type=search], input")?.focus();
        else moveFocus(list, -1);
        break;
      }
      case "Home":
        moveFocus(list, "first");
        break;
      case "End":
        moveFocus(list, "last");
        break;
      case "Enter":
      case " ": {
        const target = (event.target as HTMLElement).closest<HTMLElement>('[role="option"]');
        const model = rows.find((m) => m.id === target?.dataset.modelId);
        if (model && model.agent.kind !== "unsupported") choose(model);
        break;
      }
      default:
        return;
    }
    event.preventDefault();
  };

  const providers = availableProviders(status);
  const providerOptions = providers.map((id) => {
    const c = connectionOf(status, id);
    return {
      id,
      label: PROVIDER_NAMES[id],
      description: id === "scripted" ? undefined : c.credentialError ? "Credential storage error" : c.configured ? "API key configured" : "No API key",
    };
  });
  const focusId = visible.find((m) => m.id === value.model && m.agent.kind !== "unsupported")?.id ?? visible.find((m) => m.agent.kind !== "unsupported")?.id;
  // What the model accepts is known only from a loaded catalog that lists it.
  const known = catalog !== null && selected !== null && !placeholder;
  const efforts = known ? selected.capabilities.efforts : ALL_EFFORTS;
  const suggested = catalog?.suggested ?? null;
  const fetched = catalog?.fetchedAtMs ? relativeTime(catalog.fetchedAtMs) : null;

  return (
    <div className="agents-stack" data-model-picker data-testid={testId ?? "model-picker"} data-provider={provider}>
      <Select
        label="Provider"
        value={provider}
        onChange={switchProvider}
        options={providerOptions}
        description={provider === "scripted" ? undefined : connection.context ?? undefined}
        data-testid="model-provider"
      />
      {provider === "scripted" ? (
        <p className="ui-meta">Replays development scripts; there is no model to choose.</p>
      ) : !connection.configured ? (
        <UnavailableState
          size="compact"
          icon={KeyRound}
          title={`No ${PROVIDER_NAMES[provider]} API key`}
          description={
            <>
              {connection.credentialError ?? "Add a key to list this provider's models."}
              {value.model && (
                <>
                  {" "}
                  Chosen model: <code className="ui-code">{value.model}</code>
                </>
              )}
            </>
          }
          actions={
            onConfigure && (
              <Button size="sm" onPress={() => onConfigure(provider)} data-testid="model-configure">
                Add API key…
              </Button>
            )
          }
        />
      ) : (
        <>
          <div className="agents-row agents-row--end">
            <SearchField
              label="Model"
              placeholder="Search by name or ID"
              value={query}
              onChange={setQuery}
              className="agents-grow"
              onKeyDown={(event) => {
                if (event.key === "ArrowDown" && listRef.current) {
                  event.preventDefault();
                  const target =
                    listRef.current.querySelector<HTMLElement>('[role="option"][tabindex="0"]') ??
                    listRef.current.querySelector<HTMLElement>('[role="option"]:not([aria-disabled="true"])');
                  target?.focus();
                }
              }}
              data-testid="model-search"
            />
            <IconButton icon={RefreshCw} aria-label="Refresh models" isDisabled={loading} onPress={refresh} data-testid="model-refresh" />
          </div>
          <CatalogNotice
            state={catalog?.state ?? null}
            reason={catalog?.error ?? null}
            fetched={fetched}
            checked={catalog?.metadataChecked ?? null}
            provider={PROVIDER_NAMES[provider]}
            onRefresh={refresh}
            loading={loading}
          />
          {error ? (
            <ErrorState
              size="compact"
              live={false}
              title="The model list could not be loaded"
              description={error}
              actions={
                <Button size="sm" onPress={refresh}>
                  Try again
                </Button>
              }
            />
          ) : loading && !catalog ? (
            <LoadingState size="compact" title="Loading models…" />
          ) : rows.length === 0 ? (
            <EmptyState size="compact" title="This connection lists no models." />
          ) : visible.length === 0 ? (
            <EmptyState size="compact" title={`No models match “${query.trim()}”.`} />
          ) : (
            <div
              ref={listRef}
              role="listbox"
              aria-label="Models"
              aria-busy={loading || undefined}
              className="agents-model-list"
              onKeyDown={onListKey}
              data-testid="model-list"
            >
              {visible.map((m) => (
                <ModelRow
                  key={m.id}
                  model={m}
                  selected={m.id === value.model}
                  focusable={m.id === focusId}
                  suggested={suggested}
                  custom={custom.has(m.id)}
                  onSelect={() => choose(m)}
                />
              ))}
            </div>
          )}
          {suggested && catalog?.suggestedReason && (!value.model || value.model === suggested) && (
            <p className="ui-meta" data-testid="model-suggested">
              Suggested: {catalog.models.find((m) => m.id === suggested)?.displayName ?? suggested}. {catalog.suggestedReason}
            </p>
          )}
          {selected && !placeholder && selected.agent.kind === "unverified" && (
            <Banner tone="warning" title="Not verified for agents" data-testid="model-unverified">
              <p>{selected.agent.reason} Tool use or attachments may fail with it.</p>
              <Checkbox
                isSelected={ack}
                onChange={(on) =>
                  setAcknowledged((s) => {
                    const next = new Set(s);
                    if (on) next.add(choiceKey(value));
                    else next.delete(choiceKey(value));
                    return next;
                  })
                }
                data-testid="model-acknowledge"
              >
                Use this unverified model
              </Checkbox>
            </Banner>
          )}
          {selected && selected.agent.kind === "unsupported" && (
            <Banner tone="danger" title="This model cannot run agents" data-testid="model-unsupported">
              {selected.agent.reason} Choose another model.
            </Banner>
          )}
        </>
      )}
      {provider !== "scripted" && value.model && (
        <div className="agents-stack">
          {efforts.length > 0 ? (
            <Select<Effort>
              label="Effort"
              value={efforts.includes(value.effort) ? value.effort : null}
              onChange={(effort) => onChange({ ...value, effort })}
              options={efforts.map((id) => ({ id, label: EFFORT_LABELS[id] }))}
              description={
                catalog !== null && placeholder
                  ? `Exposition does not know which levels ${value.model} accepts. Levels are specific to ${PROVIDER_NAMES[provider]}.`
                  : `Levels are specific to ${PROVIDER_NAMES[provider]}: the same name can behave differently with another provider.`
              }
              data-testid="model-effort"
            />
          ) : (
            selected && (
              <p className="ui-meta" data-testid="model-no-effort">
                {selected.displayName} has no effort setting.
              </p>
            )
          )}
        </div>
      )}
      {provider !== "scripted" && (
        <Section title="Custom model ID" collapsible defaultExpanded={false} className="ui-inset-reset" data-testid="model-custom">
          <div className="agents-row agents-row--end">
            <TextField
              label="Exact model ID"
              code
              value={customId}
              onChange={(v) => {
                setCustomId(v);
                setCustomError(null);
              }}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  applyCustom();
                }
              }}
              isInvalid={customError !== null}
              errorMessage={customError ?? undefined}
              className="agents-grow"
              data-testid="model-custom-input"
            />
            <Button size="md" isDisabled={!customId.trim()} onPress={applyCustom} data-testid="model-custom-use">
              Use this ID
            </Button>
          </div>
          <p className="ui-meta">For a model this connection does not list. Exposition checks it when a task starts.</p>
        </Section>
      )}
    </div>
  );
}

/**
 * A message's model in a composer: the inherited choice in one line, and the
 * picker once the person chooses explicitly. `draft.model` is `null` while
 * inherited; choosing makes it explicit, and the reset button returns to the
 * inherited one.
 */
export function ModelChoice({
  value,
  inherited,
  inheritedLabel,
  resetLabel,
  chooseLabel,
  onChange,
  status,
  onProblemChange,
  onConfigure,
  testId,
}: {
  /** The explicit choice, or `null` to inherit. */
  value: ProviderChoice | null;
  /** What is used when nothing is chosen (the default, or the task's model). */
  inherited: ProviderChoice;
  /** Prefix for the inherited line, e.g. "Default". */
  inheritedLabel: string | null;
  resetLabel: string;
  chooseLabel: string;
  onChange: (value: ProviderChoice | null) => void;
  status: ProviderStatus | null;
  onProblemChange?: (problem: string | null) => void;
  onConfigure?: (provider: ProviderKind) => void;
  testId?: string;
}) {
  const labelId = useId();
  const shown = value ?? inherited;
  const summary = choiceParts(shown, (e) => EFFORT_LABELS[e]).join(" · ");
  useEffect(() => {
    if (value === null) onProblemChange?.(null);
  }, [value, onProblemChange]);
  return (
    <div className="agents-stack" role="group" aria-labelledby={labelId} data-testid={testId} data-explicit={value !== null || undefined}>
      <div className="agents-row">
        <span className="ui-field__label" id={labelId}>
          Model
        </span>
        <span className="agents-grow" data-testid="model-choice-summary">
          {value === null && inheritedLabel ? `${inheritedLabel}: ${summary}` : summary}
        </span>
        {value === null ? (
          <Button size="sm" variant="ghost" onPress={() => onChange({ ...inherited })} data-testid="model-choice-open">
            {chooseLabel}
          </Button>
        ) : (
          <Button size="sm" variant="ghost" onPress={() => onChange(null)} data-testid="model-choice-reset">
            {resetLabel}
          </Button>
        )}
      </div>
      {value !== null && (
        <ModelPicker value={value} onChange={onChange} status={status} onProblemChange={onProblemChange} onConfigure={onConfigure} />
      )}
    </div>
  );
}

/** Where the list comes from and how fresh it is; stale and unavailable lists say why. */
function CatalogNotice({
  state,
  reason,
  fetched,
  checked,
  provider,
  onRefresh,
  loading,
}: {
  state: "fresh" | "stale" | "unavailable" | null;
  reason: string | null;
  fetched: string | null;
  checked: string | null;
  provider: string;
  onRefresh: () => void;
  loading: boolean;
}) {
  if (state === "stale") {
    return (
      <Banner
        tone="stale"
        title="The model list may be out of date"
        data-testid="model-stale"
        actions={
          <Button size="sm" variant="ghost" isPending={loading} onPress={onRefresh}>
            Refresh
          </Button>
        }
      >
        {[fetched ? `Fetched ${fetched}.` : null, reason].filter(Boolean).join(" ")}
      </Banner>
    );
  }
  if (state === "unavailable") {
    return (
      <Banner
        tone="warning"
        title={`${provider} did not list its models`}
        data-testid="model-unavailable"
        actions={
          <Button size="sm" variant="ghost" isPending={loading} onPress={onRefresh}>
            Try again
          </Button>
        }
      >
        {reason ?? "The model list is not available."} Your chosen model stays selected.
      </Banner>
    );
  }
  if (state === "fresh") {
    return (
      <p className="ui-meta" data-testid="model-fresh">
        {[fetched ? `Listed by ${provider} ${fetched}.` : `Listed by ${provider}.`, checked ? `Support information checked ${checked}.` : null]
          .filter(Boolean)
          .join(" ")}
      </p>
    );
  }
  return null;
}
