// Typed graph changes from the UI. Every change goes through the backend:
// propose (validate, nothing applied) → show effects and diagnostics → apply a
// valid proposal by id with the revision the reviewer saw. Invalid requests
// explain why and never mutate anything; nothing is drawn optimistically.
import { ArrowLeftRight, CircleAlert, CircleCheck, Info, TriangleAlert } from "lucide-react";
import { useEffect, useMemo, useState } from "react";

import { notify } from "../../app/notifications";
import type { ApplyOutcome } from "../../bindings/ApplyOutcome";
import type { ChangeProposal } from "../../bindings/ChangeProposal";
import type { Entity } from "../../bindings/Entity";
import type { EntityKind } from "../../bindings/EntityKind";
import type { GraphDiagnostic } from "../../bindings/GraphDiagnostic";
import type { GraphOperation } from "../../bindings/GraphOperation";
import type { RelationKind } from "../../bindings/RelationKind";
import { Badge, Button, Dialog, Select, TextField, type SelectOption } from "../../ui/components";
import { errorText, graphApi } from "./api";
import { endpointAllowed, entityDisplay, kindDisplay, relationDisplay, type VocabularyIndex } from "./model";
import { graph, useGraph } from "./store";

export type ChangeResult =
  | { kind: "applied"; outcome: Extract<ApplyOutcome, { kind: "applied" }> }
  | { kind: "refused"; diagnostics: GraphDiagnostic[]; message: string }
  | { kind: "error"; message: string };

const firstError = (diagnostics: GraphDiagnostic[]) =>
  diagnostics.find((d) => d.severity === "error")?.message ?? diagnostics[0]?.message ?? "The change was refused.";

/** Apply a reviewed, valid proposal; reports the revision to the graph store. */
export async function applyProposal(proposal: ChangeProposal): Promise<ChangeResult> {
  try {
    const outcome = await graphApi().apply({ proposal: proposal.id, expectedRevision: proposal.baseRevision });
    if (outcome.kind === "applied") {
      graph.noteRevision(outcome.revision);
      return { kind: "applied", outcome };
    }
    graph.noteRevision(outcome.currentRevision);
    const message =
      outcome.kind === "conflict"
        ? `The graph changed while you reviewed this change: ${firstError(outcome.diagnostics)}`
        : firstError(outcome.diagnostics);
    return { kind: "refused", diagnostics: outcome.diagnostics, message };
  } catch (error) {
    return { kind: "error", message: errorText(error) };
  }
}

/**
 * Propose and, when valid, apply operations in one step — for direct actions
 * whose effect the user already chose explicitly (accept, reject, remove after
 * confirmation). Refusals are reported with the backend's reason.
 */
export async function runChange(operations: GraphOperation[], summary: string, success?: string): Promise<ChangeResult> {
  let proposal: ChangeProposal;
  try {
    proposal = await graphApi().propose({ baseRevision: useGraph.getState().revision, operations, summary });
  } catch (error) {
    const message = errorText(error);
    notify({ tone: "danger", title: `Could not ${summary.toLowerCase()}`, body: message });
    return { kind: "error", message };
  }
  if (!proposal.valid) {
    const message = firstError(proposal.diagnostics);
    notify({ tone: "warning", title: `Could not ${summary.toLowerCase()}`, body: message });
    return { kind: "refused", diagnostics: proposal.diagnostics, message };
  }
  const result = await applyProposal(proposal);
  if (result.kind === "applied") {
    if (success) notify({ tone: "success", title: success });
  } else {
    notify({ tone: result.kind === "error" ? "danger" : "warning", title: `Could not ${summary.toLowerCase()}`, body: result.message });
  }
  return result;
}

const SEVERITY_ICON = { error: CircleAlert, warning: TriangleAlert, info: Info } as const;

/** Effects of a proposal and its diagnostics, as reviewed before applying. */
export function ProposalReview({ proposal, busy, error }: { proposal: ChangeProposal | null; busy: boolean; error: string | null }) {
  if (error) {
    return (
      <div className="xp-graph-review" data-state="error" role="alert">
        <p className="xp-graph-review__line" data-severity="error">
          <CircleAlert size={14} aria-hidden /> {error}
        </p>
      </div>
    );
  }
  if (!proposal) return <p className="xp-graph-review xp-muted">{busy ? "Checking the change…" : "Choose what to change."}</p>;
  return (
    <div className="xp-graph-review" data-state={proposal.valid ? "valid" : "invalid"} data-testid="graph-proposal-review" aria-live="polite">
      {proposal.valid ? (
        <>
          <p className="xp-graph-review__heading">
            <CircleCheck size={14} aria-hidden /> Valid. Applying will:
          </p>
          <ul className="xp-graph-review__effects">
            {proposal.effects.map((effect) => (
              <li key={effect}>{effect}</li>
            ))}
          </ul>
        </>
      ) : (
        <p className="xp-graph-review__heading" data-severity="error">
          <CircleAlert size={14} aria-hidden /> Not allowed. Nothing will change.
        </p>
      )}
      {proposal.diagnostics.length > 0 && (
        <ul className="xp-graph-review__diagnostics" aria-label="Diagnostics">
          {proposal.diagnostics.map((d, i) => {
            const Icon = SEVERITY_ICON[d.severity];
            return (
              <li key={`${d.code}-${i}`} className="xp-graph-review__line" data-severity={d.severity} data-code={d.code}>
                <Icon size={14} aria-hidden />
                <span>{d.message}</span>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

/** Propose `operations` whenever they change (debounced); null operations = nothing to check yet. */
export function useProposal(operations: GraphOperation[] | null, summary: string) {
  const [proposal, setProposal] = useState<ChangeProposal | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const key = operations ? JSON.stringify(operations) : null;
  useEffect(() => {
    setProposal(null);
    setError(null);
    if (!key) return;
    let cancelled = false;
    setBusy(true);
    const timer = setTimeout(() => {
      graphApi()
        .propose({ baseRevision: useGraph.getState().revision, operations: JSON.parse(key) as GraphOperation[], summary })
        .then(
          (p) => !cancelled && setProposal(p),
          (e: unknown) => !cancelled && setError(errorText(e)),
        )
        .finally(() => !cancelled && setBusy(false));
    }, 150);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [key, summary]);
  return { proposal, busy, error, setError };
}

interface ApplyFooterProps {
  proposal: ChangeProposal | null;
  busy: boolean;
  applyLabel: string;
  onClose: () => void;
  onApplied: (outcome: Extract<ApplyOutcome, { kind: "applied" }>) => void;
  onRefused: (message: string) => void;
}

function ApplyFooter({ proposal, busy, applyLabel, onClose, onApplied, onRefused }: ApplyFooterProps) {
  const [applying, setApplying] = useState(false);
  return (
    <>
      <Button variant="secondary" onPress={onClose}>
        Cancel
      </Button>
      <Button
        variant="primary"
        isDisabled={!proposal?.valid || busy}
        isPending={applying}
        onPress={async () => {
          if (!proposal?.valid) return;
          setApplying(true);
          const result = await applyProposal(proposal);
          setApplying(false);
          if (result.kind === "applied") onApplied(result.outcome);
          else onRefused(result.message);
        }}
      >
        {applyLabel}
      </Button>
    </>
  );
}

// ----- relate two entities ---------------------------------------------------------------

export interface RelateDialogProps {
  vocabulary: VocabularyIndex | null;
  from: Entity;
  /** The other endpoint; null lets the user pick one from `candidates`. */
  to: Entity | null;
  candidates: Entity[];
  onClose: () => void;
  onApplied?: (outcome: Extract<ApplyOutcome, { kind: "applied" }>) => void;
}

/** Relation kinds people may add, those the vocabulary lists for this pair first. */
export function relationOptions(vocabulary: VocabularyIndex | null, from: Entity, to: Entity | null): SelectOption<RelationKind>[] {
  if (!vocabulary) return [];
  const manual = [...vocabulary.relationKinds.values()].filter((r) => r.manual);
  const fromTitle = entityDisplay(vocabulary, from).typeLabel;
  const toTitle = to ? entityDisplay(vocabulary, to).typeLabel : "";
  const allowed = to ? manual.filter((r) => endpointAllowed(r, from.kind, to.kind)) : manual;
  const others = to ? manual.filter((r) => !endpointAllowed(r, from.kind, to.kind)) : [];
  return [
    ...allowed.map((r) => ({ id: r.kind, label: r.reading, description: r.rule })),
    ...others.map((r) => ({ id: r.kind, label: `${r.reading} (not listed for ${fromTitle} → ${toTitle})`, description: r.rule })),
  ];
}

export function RelateDialog({ vocabulary, from: initialFrom, to: initialTo, candidates, onClose, onApplied }: RelateDialogProps) {
  const [ends, setEnds] = useState<{ from: Entity; to: Entity | null }>({ from: initialFrom, to: initialTo });
  const options = useMemo(() => relationOptions(vocabulary, ends.from, ends.to), [vocabulary, ends]);
  const [kind, setKind] = useState<RelationKind | null>(null);
  // Default to the first kind the vocabulary lists for this pair.
  useEffect(() => {
    if (ends.to && (!kind || !options.some((o) => o.id === kind))) setKind(options[0]?.id ?? null);
  }, [options, ends.to, kind]);
  const operations: GraphOperation[] | null =
    kind && ends.to
      ? [{ op: "addRelation", kind, from: { kind: "id", id: ends.from.id }, to: { kind: "id", id: ends.to.id }, evidenceKind: null, evidence: [], note: null }]
      : null;
  const { proposal, busy, error, setError } = useProposal(operations, "Add a relationship");
  const reading = kind ? relationDisplay(vocabulary, kind).reading : "…";
  const name = (e: Entity) => `${e.label} (${entityDisplay(vocabulary, e).typeLabel})`;

  return (
    <Dialog
      title="Add a relationship"
      isOpen
      onOpenChange={(open) => !open && onClose()}
      size="md"
      footer={
        <ApplyFooter
          proposal={proposal}
          busy={busy}
          applyLabel="Add relationship"
          onClose={onClose}
          onRefused={(message) => setError(message)}
          onApplied={(outcome) => {
            notify({ tone: "success", title: "Relationship added", body: `${ends.from.label} ${reading} ${ends.to?.label ?? ""}` });
            onApplied?.(outcome);
            onClose();
          }}
        />
      }
    >
      <div className="xp-graph-form" data-testid="graph-relate-dialog">
        <p className="xp-graph-form__ends">
          <Badge>{entityDisplay(vocabulary, ends.from).typeLabel}</Badge> <strong>{ends.from.label}</strong>
          <span className="xp-graph-form__reading">{reading}</span>
          {ends.to ? (
            <>
              <Badge>{entityDisplay(vocabulary, ends.to).typeLabel}</Badge> <strong>{ends.to.label}</strong>
            </>
          ) : (
            <span className="xp-muted">(choose below)</span>
          )}
        </p>
        {!initialTo && (
          <Select<string>
            label="Related item"
            placeholder="Choose an item"
            options={candidates.filter((c) => c.id !== ends.from.id).map((c) => ({ id: c.id, label: name(c) }))}
            value={ends.to?.id ?? null}
            onChange={(id) => setEnds((e) => ({ ...e, to: candidates.find((c) => c.id === id) ?? null }))}
          />
        )}
        <div className="xp-graph-form__row">
          <Select<RelationKind>
            label="Relationship"
            placeholder="Choose a relationship"
            options={options}
            value={kind}
            onChange={setKind}
            isDisabled={!ends.to}
          />
          <Button
            variant="ghost"
            icon={ArrowLeftRight}
            isDisabled={!ends.to}
            onPress={() => setEnds((e) => (e.to ? { from: e.to, to: e.from } : e))}
          >
            Swap direction
          </Button>
        </div>
        <ProposalReview proposal={proposal} busy={busy} error={error} />
      </div>
    </Dialog>
  );
}

// ----- create an entity ---------------------------------------------------------------------

export interface CreateEntityDialogProps {
  vocabulary: VocabularyIndex | null;
  /** Preselected kind (e.g. "collection" for New collection…). */
  kind?: EntityKind;
  /** Add the new entity to this collection as well. */
  memberOf?: Entity | null;
  onClose: () => void;
  onApplied?: (created: string | null) => void;
}

export function CreateEntityDialog({ vocabulary, kind: initialKind, memberOf, onClose, onApplied }: CreateEntityDialogProps) {
  const kinds = useMemo(
    () =>
      vocabulary
        ? [...vocabulary.entityKinds.values()].filter((k) => k.creatable).sort((a, b) => a.title.localeCompare(b.title))
        : [],
    [vocabulary],
  );
  const [kind, setKind] = useState<EntityKind | null>(initialKind ?? null);
  const [label, setLabel] = useState("");
  const operations: GraphOperation[] | null =
    kind && label.trim()
      ? [
          { op: "createEntity", tempId: "new", kind, roles: [], label: label.trim(), key: null, attributes: {}, binding: null },
          ...(memberOf
            ? [
                {
                  op: "addRelation" as const,
                  kind: "memberOf" as const,
                  from: { kind: "temp" as const, tempId: "new" },
                  to: { kind: "id" as const, id: memberOf.id },
                  evidenceKind: null,
                  evidence: [],
                  note: null,
                },
              ]
            : []),
        ]
      : null;
  const { proposal, busy, error, setError } = useProposal(operations, "Create an item");
  const title = initialKind ? `New ${kindDisplay(vocabulary, initialKind).title.toLowerCase()}` : "New item";
  return (
    <Dialog
      title={title}
      isOpen
      onOpenChange={(open) => !open && onClose()}
      footer={
        <ApplyFooter
          proposal={proposal}
          busy={busy}
          applyLabel="Create"
          onClose={onClose}
          onRefused={(message) => setError(message)}
          onApplied={(outcome) => {
            notify({ tone: "success", title: `Created ${label.trim()}` });
            onApplied?.(outcome.created[0]?.id ?? null);
            onClose();
          }}
        />
      }
    >
      <div className="xp-graph-form" data-testid="graph-create-dialog">
        {!initialKind && (
          <Select<EntityKind>
            label="Kind"
            placeholder="Choose a kind"
            options={kinds.map((k) => ({ id: k.kind, label: k.title }))}
            value={kind}
            onChange={setKind}
          />
        )}
        <TextField label="Name" value={label} onChange={setLabel} autoFocus />
        {memberOf && <p className="xp-muted">It will also be added to {memberOf.label}.</p>}
        <p className="xp-muted">Creates a graph item only; no file is created or changed.</p>
        <ProposalReview proposal={proposal} busy={busy} error={error} />
      </div>
    </Dialog>
  );
}

// ----- add to a collection ---------------------------------------------------------------------

export function AddToCollectionDialog({
  vocabulary,
  entity,
  collections,
  onClose,
}: {
  vocabulary: VocabularyIndex | null;
  entity: Entity;
  collections: Entity[];
  onClose: () => void;
}) {
  const NEW = "\u0000new";
  const [target, setTarget] = useState<string | null>(collections[0]?.id ?? NEW);
  const [name, setName] = useState("");
  const relation = {
    op: "addRelation" as const,
    kind: "memberOf" as const,
    from: { kind: "id" as const, id: entity.id },
    evidenceKind: null,
    evidence: [],
    note: null,
  };
  const operations: GraphOperation[] | null =
    target === NEW
      ? name.trim()
        ? [
            { op: "createEntity", tempId: "collection", kind: "collection", roles: [], label: name.trim(), key: null, attributes: {}, binding: null },
            { ...relation, to: { kind: "temp", tempId: "collection" } },
          ]
        : null
      : target
        ? [{ ...relation, to: { kind: "id", id: target } }]
        : null;
  const { proposal, busy, error, setError } = useProposal(operations, "Add to a collection");
  return (
    <Dialog
      title={`Add ${entity.label} to a collection`}
      isOpen
      onOpenChange={(open) => !open && onClose()}
      footer={
        <ApplyFooter
          proposal={proposal}
          busy={busy}
          applyLabel="Add to collection"
          onClose={onClose}
          onRefused={(message) => setError(message)}
          onApplied={() => {
            notify({ tone: "success", title: `Added ${entity.label} to a collection` });
            onClose();
          }}
        />
      }
    >
      <div className="xp-graph-form" data-testid="graph-collection-dialog">
        <Select<string>
          label="Collection"
          options={[
            ...collections.map((c) => ({ id: c.id, label: `${c.label} (${entityDisplay(vocabulary, c).typeLabel})` })),
            { id: NEW, label: "New collection…" },
          ]}
          value={target}
          onChange={setTarget}
        />
        {target === NEW && <TextField label="New collection name" value={name} onChange={setName} />}
        <p className="xp-muted">Collections organize existing items without copying them.</p>
        <ProposalReview proposal={proposal} busy={busy} error={error} />
      </div>
    </Dialog>
  );
}
