// Inspector for the selected graph item: entity detail (bindings, typed
// relationships, evidence, provenance), change impact, and the actions that
// apply to it. View-only actions ("Hide from view") and semantic ones
// ("Remove relationship") are worded and placed apart; nothing here deletes a
// file.
import {
  Archive,
  ArchiveRestore,
  Check,
  ChevronRight,
  Crosshair,
  EyeOff,
  Eye,
  FileSearch,
  FolderPlus,
  GitBranchPlus,
  ListChecks,
  MoreHorizontal,
  Trash2,
  X,
} from "lucide-react";
import { useState, type ReactNode } from "react";

import type { Entity } from "../../bindings/Entity";
import type { EntityDetail } from "../../bindings/EntityDetail";
import type { Evidence } from "../../bindings/Evidence";
import type { Freshness } from "../../bindings/Freshness";
import type { ImpactReport } from "../../bindings/ImpactReport";
import type { Relation } from "../../bindings/Relation";
import type { SourceBinding } from "../../bindings/SourceBinding";
import {
  Badge,
  Button,
  confirm,
  EmptyState,
  ErrorState,
  IconButton,
  LoadingState,
  Menu,
  MenuItem,
  MenuTrigger,
  Section,
  StatusPill,
  Toolbar,
  type BadgeTone,
} from "../../ui/components";
import { errorText, graphApi } from "./api";
import { runChange } from "./changes";
import {
  entityDisplay,
  FAMILY_LABELS,
  freshnessView,
  originLabel,
  relationDisplay,
  STATUS_LABELS,
  VERDICT_VIEW,
  type VocabularyIndex,
} from "./model";
import { revealSource } from "./reveal";
import { useGraphLoad } from "./useGraphData";

export interface InspectorActions {
  /** Select another entity (e.g. a neighbor) in the current view. */
  onSelectEntity?: (id: string) => void;
  /** Show the local neighborhood of this entity. */
  onFocus?: (id: string) => void;
  onRelate?: (entity: Entity) => void;
  onAddToCollection?: (entity: Entity) => void;
  /** View-only hide/show. */
  onHide?: (id: string) => void;
  onUnhide?: (id: string) => void;
  isHidden?: (id: string) => boolean;
  /** Graph changes may be proposed (attached, not read-only). */
  canWrite: boolean;
}

const STATUS_TONE: Record<Relation["status"], BadgeTone> = {
  accepted: "neutral",
  suggested: "accent",
  observed: "info",
  rejected: "neutral",
};

export function RelationStatusBadge({ status }: { status: Relation["status"] }) {
  return (
    <Badge tone={STATUS_TONE[status]} dashed={status !== "accepted"} title={statusTitle(status)}>
      {STATUS_LABELS[status]}
    </Badge>
  );
}

function statusTitle(status: Relation["status"]): string {
  switch (status) {
    case "accepted":
      return "An accepted assertion";
    case "suggested":
      return "Suggested, not yet accepted";
    case "observed":
      return "Observed in the sources by indexing";
    case "rejected":
      return "A rejected suggestion, kept for provenance";
  }
}

const BINDING_TONE: Record<SourceBinding["state"], BadgeTone> = {
  resolved: "neutral",
  missing: "warning",
  ambiguous: "warning",
  unresolved: "warning",
};

export function BindingRow({ binding }: { binding: SourceBinding }) {
  const where = binding.range ? `characters ${binding.range.start}–${binding.range.end}` : "whole file";
  const unsaved = binding.version && !binding.version.saved;
  return (
    <li className="xp-graph-list__item" data-testid="graph-binding" data-path={binding.path} data-state={binding.state}>
      <div className="xp-graph-list__main">
        <code className="xp-graph-path">{binding.path}</code>
        <span className="xp-muted"> · {where}</span>
        <div className="xp-graph-badges">
          <Badge tone={BINDING_TONE[binding.state]} dashed={binding.state !== "resolved"}>
            {binding.state === "resolved" ? "Resolved" : binding.state === "missing" ? "Missing" : binding.state === "ambiguous" ? "Ambiguous" : "Unresolved"}
          </Badge>
          {unsaved && <Badge tone="info" dashed>Unsaved buffer</Badge>}
        </div>
        {binding.detail && <p className="xp-muted xp-graph-list__detail">{binding.detail}</p>}
      </div>
      <Button size="sm" variant="ghost" icon={FileSearch} isDisabled={binding.state !== "resolved"} onPress={() => void revealSource(binding.path, binding.range)}>
        Open source
      </Button>
    </li>
  );
}

function RelationRow({
  relation,
  outgoing,
  other,
  vocabulary,
  actions,
}: {
  relation: Relation;
  outgoing: boolean;
  other: Entity | undefined;
  vocabulary: VocabularyIndex | null;
  actions: InspectorActions;
}) {
  const info = relationDisplay(vocabulary, relation.kind);
  const otherLabel = other?.label ?? "an item outside this view";
  const canEdit = actions.canWrite && info.manual && relation.status !== "observed";
  return (
    <li className="xp-graph-list__item" data-testid="graph-relation" data-relation-id={relation.id} data-kind={relation.kind} data-status={relation.status}>
      <div className="xp-graph-list__main">
        <span className="xp-graph-relation__reading">{outgoing ? info.reading : info.inverse}</span>{" "}
        {other && actions.onSelectEntity ? (
          <button type="button" className="xp-link" onClick={() => actions.onSelectEntity?.(other.id)}>
            {otherLabel}
          </button>
        ) : (
          <span>{otherLabel}</span>
        )}
        {other && <span className="xp-muted"> ({entityDisplay(vocabulary, other).typeLabel})</span>}
        <div className="xp-graph-badges">
          <RelationStatusBadge status={relation.status} />
          {relation.status === "observed" && <span className="xp-muted">from {originLabel(relation.origin)}</span>}
        </div>
      </div>
      {canEdit && <RelationActions relation={relation} description={`${outgoing ? "" : otherLabel + " "}${info.reading}${outgoing ? " " + otherLabel : ""}`} />}
    </li>
  );
}

/** Accept/Reject a suggestion; remove a relationship (semantic, confirmed). */
export function RelationActions({ relation, description }: { relation: Relation; description: string }) {
  return (
    <Toolbar aria-label="Relationship actions" className="xp-graph-list__actions">
      {relation.status === "suggested" && (
        <>
          <IconButton
            icon={Check}
            size="sm"
            aria-label="Accept suggestion"
            onPress={() => void runChange([{ op: "setRelationStatus", relation: relation.id, status: "accepted" }], "Accept the suggestion", "Suggestion accepted")}
          />
          <IconButton
            icon={X}
            size="sm"
            aria-label="Reject suggestion"
            onPress={() => void runChange([{ op: "setRelationStatus", relation: relation.id, status: "rejected" }], "Reject the suggestion", "Suggestion rejected")}
          />
        </>
      )}
      <IconButton icon={Trash2} size="sm" aria-label="Remove relationship" onPress={() => void removeRelationship(relation, description)} />
    </Toolbar>
  );
}

export async function removeRelationship(relation: Relation, description: string): Promise<void> {
  const choice = await confirm<"cancel" | "remove">({
    title: "Remove this relationship?",
    message: `“${description.trim()}” will be removed from the project graph. No file changes; the items stay.`,
    actions: [
      { id: "cancel", label: "Cancel", variant: "secondary" },
      { id: "remove", label: "Remove relationship", variant: "danger" },
    ],
    cancelId: "cancel",
    defaultId: "cancel",
    tone: "warning",
  });
  if (choice === "remove") await runChange([{ op: "removeRelation", relation: relation.id }], "Remove the relationship", "Relationship removed");
}

function freshnessOf(detail: EntityDetail, id: string): Freshness | null {
  return detail.evidenceFreshness.find(([e]) => e === id)?.[1] ?? null;
}

export function EvidenceRow({ evidence, freshness }: { evidence: Evidence; freshness: Freshness | null }) {
  const fresh = freshnessView(freshness);
  const outcome = evidence.outcome;
  const formal = evidence.formal;
  return (
    <li className="xp-graph-evidence" data-testid="graph-evidence" data-kind={evidence.kind}>
      <div className="xp-graph-badges">
        <Badge>{evidence.kind === "formalCheck" ? "Formal check" : evidence.kind.charAt(0).toUpperCase() + evidence.kind.slice(1)}</Badge>
        {outcome.kind === "verdict" ? (
          <StatusPill size="sm" status={VERDICT_VIEW[outcome.verdict].status} label={VERDICT_VIEW[outcome.verdict].label} />
        ) : outcome.kind === "failed" ? (
          <StatusPill size="sm" status="failed" label="Failed" />
        ) : outcome.kind === "succeeded" ? (
          <StatusPill size="sm" status="succeeded" label="Succeeded" />
        ) : outcome.kind === "reported" ? (
          <Badge>Reported</Badge>
        ) : (
          <StatusPill size="sm" status="canceled" label={outcome.kind === "canceled" ? "Canceled" : "Interrupted"} />
        )}
        <StatusPill size="sm" status={fresh.status} label={fresh.label} />
      </div>
      <p className="xp-muted">
        {evidence.operation.kind} {evidence.operation.id} · recorded {new Date(Number(evidence.recordedAtMs)).toLocaleString()} by {originLabel(evidence.recordedBy)}
      </p>
      {freshness && freshness.kind !== "current" && <p className="xp-muted">{fresh.detail}</p>}
      {formal && <FormalEvidenceBody formal={formal} />}
    </li>
  );
}

export function FormalEvidenceBody({ formal }: { formal: NonNullable<Evidence["formal"]> }) {
  const declarationVerdict = formal.declarationVerdict;
  return (
    <div className="xp-graph-formal" data-testid="graph-formal-evidence">
      <p>
        Lean checked <code>{formal.declarationKind} {formal.declaration}</code> in <code>{formal.sourcePath}</code>
        {formal.revision !== null ? ` (revision ${formal.revision})` : ""} with {formal.toolchain}.
      </p>
      <pre className="xp-graph-formal__statement" aria-label="Checked statement">
        {formal.statement}
      </pre>
      <p>{formal.axioms.length ? `Axioms: ${formal.axioms.join(", ")}` : "No axioms."}</p>
      <p>
        File verdict: {VERDICT_VIEW[formal.fileVerdict].label}.{" "}
        {declarationVerdict
          ? `This declaration: ${VERDICT_VIEW[declarationVerdict].label}.`
          : "The audit reported no separate verdict for this declaration; only the file verdict applies."}
      </p>
      {formal.dependencies.kind === "reported" ? (
        <p>Formal dependencies reported by the checker: {formal.dependencies.names.length ? formal.dependencies.names.join(", ") : "none"}.</p>
      ) : (
        <p data-testid="graph-formal-deps-unavailable">Formal dependency edges unavailable: {formal.dependencies.reason}</p>
      )}
      {formal.dependencyFingerprints === null && <p className="xp-muted">Freshness against formal dependencies is unknown (no dependency fingerprints).</p>}
    </div>
  );
}

const ACTION_LABEL: Record<ImpactReport["items"][number]["action"], { label: string; tone: BadgeTone }> = {
  needsReview: { label: "Needs review", tone: "warning" },
  stale: { label: "Stale", tone: "stale" },
  citationReview: { label: "Citation review", tone: "info" },
  informational: { label: "Informational", tone: "neutral" },
};

/** "What needs review if this changes?" — bounded impact with reasons and paths. */
export function ImpactSection({ entityId, labelOf, onSelectEntity }: { entityId: string; labelOf: (id: string) => string; onSelectEntity?: (id: string) => void }) {
  const [report, setReport] = useState<ImpactReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const run = async () => {
    setBusy(true);
    setError(null);
    try {
      setReport(await graphApi().impact({ entities: [entityId], maxDepth: null, maxItems: null }));
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Section title="Change impact">
      <div className="xp-graph-impact" data-testid="graph-impact">
        <Button size="sm" icon={ListChecks} isPending={busy} onPress={() => void run()}>
          What needs review if this changes?
        </Button>
        {error && <p className="xp-inline-error">{error}</p>}
        {report && (
          <>
            {report.items.length === 0 ? (
              <p className="xp-muted">Nothing in the graph depends on it.</p>
            ) : (
              <ul className="xp-graph-list" aria-label="Affected items">
                {report.items.map((item) => (
                  <li key={item.entity.id} className="xp-graph-list__item" data-testid="graph-impact-item" data-entity-id={item.entity.id} data-action={item.action}>
                    <div className="xp-graph-list__main">
                      {onSelectEntity ? (
                        <button type="button" className="xp-link" onClick={() => onSelectEntity(item.entity.id)}>
                          {item.entity.label}
                        </button>
                      ) : (
                        item.entity.label
                      )}
                      <div className="xp-graph-badges">
                        <Badge tone={ACTION_LABEL[item.action].tone}>{ACTION_LABEL[item.action].label}</Badge>
                      </div>
                      <p className="xp-muted">
                        {item.reason}
                        {item.path.length > 0 &&
                          ` — via ${item.path.map((step) => `${labelOf(step.from)} → ${labelOf(step.to)}`).join(", ")}`}
                      </p>
                    </div>
                  </li>
                ))}
              </ul>
            )}
            {report.cycles.length > 0 && (
              <p className="xp-muted">
                Cycles were met and traversed once: {report.cycles.map((c) => c.map(labelOf).join(" → ")).join("; ")}.
              </p>
            )}
            {(report.truncated || report.workLimitReached) && (
              <p className="xp-muted">The analysis stopped at its limit; more items may be affected.</p>
            )}
            {report.unknownCoverage.length > 0 && (
              <p className="xp-muted">Impact through {report.unknownCoverage.join(", ")} is unknown (incomplete extraction).</p>
            )}
            <p className="xp-muted">A change never makes a result false by itself; it marks what to review.</p>
          </>
        )}
      </div>
    </Section>
  );
}

export interface EntityInspectorProps {
  entityId: string;
  vocabulary: VocabularyIndex | null;
  actions: InspectorActions;
  /** Extra sections (e.g. Mathematics). */
  children?: (detail: EntityDetail) => ReactNode;
}

export function EntityInspector({ entityId, vocabulary, actions, children }: EntityInspectorProps) {
  const loaded = useGraphLoad(`entity:${entityId}`, () => graphApi().entity(entityId));
  const detail = loaded.data;
  if (!detail) {
    if (loaded.error) return <ErrorState title="Could not load this item" description={loaded.error} actions={<Button onPress={loaded.reload}>Retry</Button>} />;
    return <LoadingState title="Loading…" />;
  }
  const { entity } = detail;
  const display = entityDisplay(vocabulary, entity);
  const neighbors = new Map(detail.neighbors.map((n) => [n.id, n]));
  const labelOf = (id: string) => (id === entity.id ? entity.label : neighbors.get(id)?.label ?? id);
  const hidden = actions.isHidden?.(entity.id) ?? false;
  const resolved = detail.bindings.find((b) => b.state === "resolved");
  const relations = [
    ...detail.outgoing.map((r) => ({ r, outgoing: true })),
    ...detail.incoming.map((r) => ({ r, outgoing: false })),
  ];
  const families = [...new Set(relations.map(({ r }) => relationDisplay(vocabulary, r.kind).family))];
  const archived = entity.lifecycle === "archived";
  return (
    <div className="xp-graph-inspector__body" data-testid="graph-entity-inspector" data-entity-id={entity.id}>
      <header className="xp-graph-inspector__header">
        <span className="xp-graph-inspector__type" data-category={display.category}>
          <display.icon size={14} aria-hidden /> {display.typeLabel}
        </span>
        <h3 className="xp-graph-inspector__title">{entity.label}</h3>
        <div className="xp-graph-badges">
          {entity.lifecycle !== "active" && <Badge dashed>{entity.lifecycle === "tombstoned" ? "Removed from source" : entity.lifecycle === "archived" ? "Archived" : "Discarded"}</Badge>}
          {hidden && <Badge dashed>Hidden from this view</Badge>}
          <Badge title="Who admitted it">{originLabel(entity.origin)}</Badge>
        </div>
        {entity.tombstoneReason && <p className="xp-muted">{entity.tombstoneReason}</p>}
      </header>
      <Toolbar aria-label="Item actions" className="xp-graph-inspector__actions">
        <Button size="sm" icon={FileSearch} isDisabled={!resolved} onPress={() => resolved && void revealSource(resolved.path, resolved.range)}>
          Open source
        </Button>
        {actions.onFocus && (
          <Button size="sm" variant="ghost" icon={Crosshair} onPress={() => actions.onFocus?.(entity.id)}>
            Show neighborhood
          </Button>
        )}
        {actions.onRelate && (
          <Button size="sm" variant="ghost" icon={GitBranchPlus} isDisabled={!actions.canWrite} onPress={() => actions.onRelate?.(entity)}>
            Relate to…
          </Button>
        )}
        <MenuTrigger>
          <IconButton icon={MoreHorizontal} size="sm" aria-label="More actions" />
          <Menu
            aria-label="More actions"
            onAction={(key) => {
              if (key === "collection") actions.onAddToCollection?.(entity);
              if (key === "hide") actions.onHide?.(entity.id);
              if (key === "unhide") actions.onUnhide?.(entity.id);
              if (key === "archive") void setLifecycle(entity, "archived");
              if (key === "restore") void setLifecycle(entity, "active");
            }}
          >
            {actions.onAddToCollection ? (
              <MenuItem id="collection" icon={FolderPlus} isDisabled={!actions.canWrite}>
                Add to collection…
              </MenuItem>
            ) : null}
            {actions.onHide && !hidden ? (
              <MenuItem id="hide" icon={EyeOff} description="Removes it from this view only">
                Hide from view
              </MenuItem>
            ) : null}
            {actions.onUnhide && hidden ? (
              <MenuItem id="unhide" icon={Eye}>
                Show in view
              </MenuItem>
            ) : null}
            {!display.kind.fileBacked && entity.kind !== "project" ? (
              archived ? (
                <MenuItem id="restore" icon={ArchiveRestore} isDisabled={!actions.canWrite}>
                  Restore
                </MenuItem>
              ) : (
                <MenuItem id="archive" icon={Archive} isDisabled={!actions.canWrite} description="Keeps it and its relationships; hidden by default">
                  Archive
                </MenuItem>
              )
            ) : null}
          </Menu>
        </MenuTrigger>
      </Toolbar>
      {display.kind.fileBacked && (
        <p className="xp-muted xp-graph-inspector__note">
          Hiding removes it from this view only. Deleting or renaming the underlying file is a file operation and is not done from the graph.
        </p>
      )}

      <Section title={`Source (${detail.bindings.length})`}>
        {detail.bindings.length ? (
          <ul className="xp-graph-list">
            {detail.bindings.map((b, i) => (
              <BindingRow key={`${b.path}-${i}`} binding={b} />
            ))}
          </ul>
        ) : (
          <p className="xp-muted">Not bound to a source.</p>
        )}
        {detail.coverage && (
          <p className="xp-muted" data-testid="graph-coverage">
            Extraction: {detail.coverage.state}
            {detail.coverage.extractor ? ` (${detail.coverage.extractor})` : ""}
            {detail.coverage.unresolved ? ` · ${detail.coverage.unresolved} unresolved` : ""}
            {detail.coverage.detail ? ` · ${detail.coverage.detail}` : ""}
          </p>
        )}
      </Section>

      <Section title={`Relationships (${relations.length})`}>
        {relations.length === 0 && <p className="xp-muted">No relationships.</p>}
        {families.map((family) => (
          <div key={family} className="xp-graph-family" data-family={family}>
            <h4 className="xp-graph-family__title">{FAMILY_LABELS[family]}</h4>
            <ul className="xp-graph-list">
              {relations
                .filter(({ r }) => relationDisplay(vocabulary, r.kind).family === family)
                .map(({ r, outgoing }) => (
                  <RelationRow
                    key={`${r.id}-${outgoing}`}
                    relation={r}
                    outgoing={outgoing}
                    other={neighbors.get(outgoing ? r.to : r.from)}
                    vocabulary={vocabulary}
                    actions={actions}
                  />
                ))}
            </ul>
          </div>
        ))}
      </Section>

      {children?.(detail)}

      <Section title={`Evidence (${detail.evidence.length})`}>
        {detail.evidence.length ? (
          <ul className="xp-graph-list">
            {detail.evidence.map((ev) => (
              <EvidenceRow key={ev.id} evidence={ev} freshness={freshnessOf(detail, ev.id)} />
            ))}
          </ul>
        ) : (
          <p className="xp-muted">No evidence recorded.</p>
        )}
      </Section>

      <ImpactSection entityId={entity.id} labelOf={labelOf} onSelectEntity={actions.onSelectEntity} />

      <Section title="Provenance" collapsible defaultExpanded={false}>
        <dl className="xp-graph-facts">
          <dt>Admitted by</dt>
          <dd>{originLabel(entity.origin)}</dd>
          {entity.key && (
            <>
              <dt>Key</dt>
              <dd>
                <code>{entity.key}</code>
              </dd>
            </>
          )}
          <dt>Item revision</dt>
          <dd>{entity.revision}</dd>
          <dt>Graph revision</dt>
          <dd>{detail.graphRevision}</dd>
          <dt>Id</dt>
          <dd>
            <code>{entity.id}</code>
          </dd>
        </dl>
      </Section>
    </div>
  );
}

async function setLifecycle(entity: Entity, lifecycle: "active" | "archived") {
  if (lifecycle === "archived") {
    const choice = await confirm<"cancel" | "archive">({
      title: `Archive ${entity.label}?`,
      message: "It stays in the project graph with its relationships, and is hidden from views unless you show archived items. No file changes.",
      actions: [
        { id: "cancel", label: "Cancel", variant: "secondary" },
        { id: "archive", label: "Archive", variant: "primary" },
      ],
      cancelId: "cancel",
    });
    if (choice !== "archive") return;
  }
  await runChange(
    [{ op: "setLifecycle", entity: entity.id, expectedRevision: entity.revision, lifecycle }],
    lifecycle === "archived" ? "Archive the item" : "Restore the item",
    lifecycle === "archived" ? `Archived ${entity.label}` : `Restored ${entity.label}`,
  );
}

/** Inspector for a selected relationship. */
export function RelationInspector({
  relation,
  entities,
  vocabulary,
  actions,
}: {
  relation: Relation;
  entities: Map<string, Entity>;
  vocabulary: VocabularyIndex | null;
  actions: InspectorActions;
}) {
  const info = relationDisplay(vocabulary, relation.kind);
  const from = entities.get(relation.from);
  const to = entities.get(relation.to);
  const description = `${from?.label ?? relation.from} ${info.reading} ${to?.label ?? relation.to}`;
  return (
    <div className="xp-graph-inspector__body" data-testid="graph-relation-inspector" data-relation-id={relation.id}>
      <header className="xp-graph-inspector__header">
        <span className="xp-graph-inspector__type">{FAMILY_LABELS[info.family]}</span>
        <h3 className="xp-graph-inspector__title">{description}</h3>
        <div className="xp-graph-badges">
          <RelationStatusBadge status={relation.status} />
          <Badge>{originLabel(relation.origin)}</Badge>
        </div>
      </header>
      <dl className="xp-graph-facts">
        <dt>From</dt>
        <dd>
          <button type="button" className="xp-link" onClick={() => actions.onSelectEntity?.(relation.from)}>
            {from?.label ?? relation.from} <ChevronRight size={12} aria-hidden />
          </button>
        </dd>
        <dt>To</dt>
        <dd>
          <button type="button" className="xp-link" onClick={() => actions.onSelectEntity?.(relation.to)}>
            {to?.label ?? relation.to} <ChevronRight size={12} aria-hidden />
          </button>
        </dd>
        <dt>Rule</dt>
        <dd>{info.rule}</dd>
        {relation.evidenceKind && (
          <>
            <dt>Evidence kind</dt>
            <dd>{relation.evidenceKind}</dd>
          </>
        )}
        {relation.note && (
          <>
            <dt>Note</dt>
            <dd>{relation.note}</dd>
          </>
        )}
      </dl>
      {relation.status === "observed" ? (
        <p className="xp-muted">Observed in the sources: to change it, change the source.</p>
      ) : !info.manual ? (
        <p className="xp-muted">This kind of relationship follows the files; it is not edited by hand.</p>
      ) : actions.canWrite ? (
        <RelationActions relation={relation} description={description} />
      ) : null}
    </div>
  );
}

export function InspectorEmpty({ counts }: { counts?: string }) {
  return <EmptyState title="Nothing selected" description={`Select an item or relationship to inspect it.${counts ? ` ${counts}` : ""}`} />;
}
