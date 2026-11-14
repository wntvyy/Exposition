// Inspector for the selected graph item: entity detail (bindings, typed
// relationships, evidence, provenance), change impact, and the actions that
// apply to it. View-only actions ("Hide from view"), semantic ones ("Delete
// relationship", "Delete entity") and file ones ("Delete source file") are
// worded and placed apart; deletions go through the shared operation client
// (P1), which asks only when the plan names a concrete consequence.
//
// Layout: InspectorHeader (type, title, facts, actions) over an InspectorBody
// of content Sections with counts. Status is quiet: settled facts (accepted,
// observed, resolved) read as muted text, provisional ones (suggested,
// unresolved, unsaved) as dashed badges, and outcomes/verdicts/freshness as
// StatusText with their exact labels.
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
  InspectorBody,
  InspectorHeader,
  KeyValueList,
  LoadingState,
  Menu,
  MenuItem,
  MenuSeparator,
  MenuTrigger,
  Section,
  StatusText,
  Toolbar,
  type IconComponent,
  type KeyValueItem,
  type Status,
} from "../../ui/components";
import type { GraphCategory } from "../../ui/tokens";
import { runUserOperation } from "../operations/client";
import { targetKey, trashName } from "../operations/model";
import { opMenuItems, runMenuKey } from "../operations/OpMenu";
import { errorText, graphApi } from "./api";
import { runChange } from "./changes";
import { graphActions } from "./graphOps";
import {
  entityDisplay,
  RELATION_FAMILY_CATEGORY,
  FAMILY_LABELS,
  FAMILY_ORDER,
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

/** Type indicator (icon + type text) in its category's text color: inspector headers, dependency items. */
export function TypeLabel({ category, icon: Icon, label }: { category: GraphCategory; icon?: IconComponent; label: string }) {
  return (
    <span className="xp-graph-type" data-category={category}>
      {Icon && <Icon size={12} aria-hidden />}
      {label}
    </span>
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

/**
 * A relationship's assertion status. Suggested (provisional) is a dashed
 * accent badge; settled statuses are muted text: a "· observed" suffix in a
 * relationship line (`inline`), or the plain word in a table cell or fact
 * (`text`).
 */
export function RelationStatus({ status, form = "inline" }: { status: Relation["status"]; form?: "inline" | "text" }) {
  if (status === "suggested") {
    return (
      <Badge tone="accent" dashed title={statusTitle(status)}>
        {STATUS_LABELS[status]}
      </Badge>
    );
  }
  return (
    <span className="xp-muted" title={statusTitle(status)} data-status={status}>
      {form === "inline" ? `· ${STATUS_LABELS[status].toLowerCase()}` : STATUS_LABELS[status]}
    </span>
  );
}

const BINDING_LABEL: Record<Exclude<SourceBinding["state"], "resolved">, string> = {
  missing: "Missing",
  ambiguous: "Ambiguous",
  unresolved: "Unresolved",
};

export function BindingRow({ binding }: { binding: SourceBinding }) {
  const where = binding.range ? `characters ${binding.range.start}–${binding.range.end}` : "whole file";
  const unsaved = binding.version && !binding.version.saved;
  return (
    <li className="xp-graph-row" data-testid="graph-binding" data-path={binding.path} data-state={binding.state}>
      <div className="xp-graph-row__main">
        <p className="xp-graph-row__line">
          <span className="ui-code">{binding.path}</span> <span className="xp-muted">· {where}</span>
          {binding.state === "resolved" ? (
            <span className="xp-muted"> · resolved</span>
          ) : (
            <>
              {" "}
              <Badge tone="warning" dashed>
                {BINDING_LABEL[binding.state]}
              </Badge>
            </>
          )}
          {unsaved && (
            <>
              {" "}
              <Badge tone="info" dashed>
                Unsaved buffer
              </Badge>
            </>
          )}
        </p>
        {binding.detail && <p className="ui-meta">{binding.detail}</p>}
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
    <li className="xp-graph-row" data-testid="graph-relation" data-relation-id={relation.id} data-kind={relation.kind} data-status={relation.status}>
      <p className="xp-graph-row__line xp-graph-row__main">
        {outgoing ? info.reading : info.inverse}{" "}
        {other && actions.onSelectEntity ? (
          <button type="button" className="xp-link" onClick={() => actions.onSelectEntity?.(other.id)}>
            {otherLabel}
          </button>
        ) : (
          <span>{otherLabel}</span>
        )}
        {other && <span className="xp-muted"> ({entityDisplay(vocabulary, other).typeLabel})</span>} <RelationStatus status={relation.status} />
        {relation.status === "observed" && <span className="xp-muted"> from {originLabel(relation.origin)}</span>}
      </p>
      {canEdit && <RelationActions relation={relation} description={`${outgoing ? "" : otherLabel + " "}${info.reading}${outgoing ? " " + otherLabel : ""}`} />}
    </li>
  );
}

/** Accept/Reject a suggestion; remove a relationship (semantic, confirmed). */
export function RelationActions({ relation, description }: { relation: Relation; description: string }) {
  return (
    <Toolbar aria-label="Relationship actions" className="xp-graph-row__actions">
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
      <IconButton
        icon={Trash2}
        size="sm"
        aria-label="Delete relationship"
        onPress={() => void deleteRelationship(relation, description)}
      />
    </Toolbar>
  );
}

/**
 * Delete a manual relationship through the shared operation client: applied
 * at once unless the plan names a consequence (one decision then); the items
 * and files stay. The removal persists across reindexing.
 */
export async function deleteRelationship(relation: Relation, description: string): Promise<void> {
  const target = { kind: "graphRelation" as const, relation: relation.id };
  await runUserOperation({ kind: "delete", permanent: false }, [target], { labels: new Map([[targetKey(target), description.trim()]]) });
}

function freshnessOf(detail: EntityDetail, id: string): Freshness | null {
  return detail.evidenceFreshness.find(([e]) => e === id)?.[1] ?? null;
}

/** An evidence outcome as quiet status text with its exact label. */
export function OutcomeText({ outcome }: { outcome: Evidence["outcome"] }) {
  switch (outcome.kind) {
    case "verdict":
      return <StatusText status={VERDICT_VIEW[outcome.verdict].status} label={VERDICT_VIEW[outcome.verdict].label} />;
    case "failed":
      return <StatusText status="failed" label="Failed" />;
    case "succeeded":
      return <StatusText status="succeeded" label="Succeeded" />;
    case "reported":
      return <span>Reported</span>;
    case "canceled":
    case "interrupted":
      return <StatusText status="canceled" label={outcome.kind === "canceled" ? "Canceled" : "Interrupted"} />;
  }
}

export function evidenceKindLabel(kind: Evidence["kind"]): string {
  return kind === "formalCheck" ? "Formal check" : kind.charAt(0).toUpperCase() + kind.slice(1);
}

export function EvidenceRow({ evidence, freshness }: { evidence: Evidence; freshness: Freshness | null }) {
  const fresh = freshnessView(freshness);
  const formal = evidence.formal;
  const items: KeyValueItem[] = [
    { key: "kind", label: "Kind", value: evidenceKindLabel(evidence.kind) },
    { key: "outcome", label: "Outcome", value: <OutcomeText outcome={evidence.outcome} /> },
    {
      key: "freshness",
      label: "Freshness",
      value: (
        <>
          <StatusText status={fresh.status} label={fresh.label} />
          {freshness && freshness.kind !== "current" && <span className="xp-graph-detail">{fresh.detail}</span>}
        </>
      ),
    },
    { key: "operation", label: "Operation", value: `${evidence.operation.kind} ${evidence.operation.id}`, code: true },
    { key: "recorded", label: "Recorded", value: `${new Date(Number(evidence.recordedAtMs)).toLocaleString()} by ${originLabel(evidence.recordedBy)}` },
  ];
  return (
    <li className="xp-graph-evidence" data-testid="graph-evidence" data-kind={evidence.kind}>
      <KeyValueList items={items} />
      {formal && <FormalEvidenceBody formal={formal} />}
    </li>
  );
}

export function FormalEvidenceBody({ formal }: { formal: NonNullable<Evidence["formal"]> }) {
  const declarationVerdict = formal.declarationVerdict;
  return (
    <div className="xp-graph-formal" data-testid="graph-formal-evidence">
      <p>
        Lean checked <code className="ui-code">{formal.declarationKind} {formal.declaration}</code> in <code className="ui-code">{formal.sourcePath}</code>
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

const ACTION_LABEL: Record<ImpactReport["items"][number]["action"], { label: string; status: Status }> = {
  needsReview: { label: "Needs review", status: "warning" },
  stale: { label: "Stale", status: "stale" },
  citationReview: { label: "Citation review", status: "warning" },
  informational: { label: "Informational", status: "idle" },
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
    <Section title="Change impact" count={report?.items.length}>
      <div className="xp-graph-stack" data-testid="graph-impact">
        <Button size="sm" icon={ListChecks} isPending={busy} onPress={() => void run()}>
          What needs review if this changes?
        </Button>
        {error && <p className="xp-inline-error">{error}</p>}
        {report && (
          <>
            {report.items.length === 0 ? (
              <p className="xp-muted">Nothing in the graph depends on it.</p>
            ) : (
              <ul className="xp-graph-rows" aria-label="Affected items">
                {report.items.map((item) => (
                  <li key={item.entity.id} className="xp-graph-row" data-testid="graph-impact-item" data-entity-id={item.entity.id} data-action={item.action}>
                    <div className="xp-graph-row__main">
                      <p className="xp-graph-row__line">
                        {onSelectEntity ? (
                          <button type="button" className="xp-link" onClick={() => onSelectEntity(item.entity.id)}>
                            {item.entity.label}
                          </button>
                        ) : (
                          item.entity.label
                        )}{" "}
                        <StatusText status={ACTION_LABEL[item.action].status} label={ACTION_LABEL[item.action].label} />
                      </p>
                      <p className="ui-meta">
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
              <p className="ui-meta">
                Cycles were met and traversed once: {report.cycles.map((c) => c.map(labelOf).join(" → ")).join("; ")}.
              </p>
            )}
            {(report.truncated || report.workLimitReached) && (
              <p className="ui-meta">The analysis stopped at its limit; more items may be affected.</p>
            )}
            {report.unknownCoverage.length > 0 && (
              <p className="ui-meta">Impact through {report.unknownCoverage.join(", ")} is unknown (incomplete extraction).</p>
            )}
            <p className="ui-meta">A change never makes a result false by itself; it marks what to review.</p>
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

const LIFECYCLE_LABEL: Record<Exclude<Entity["lifecycle"], "active">, string> = {
  tombstoned: "Removed from source",
  archived: "Archived",
  discarded: "Discarded",
};

export function EntityInspector({ entityId, vocabulary, actions, children }: EntityInspectorProps) {
  const loaded = useGraphLoad(`entity:${entityId}`, () => graphApi().entity(entityId));
  const detail = loaded.data;
  if (!detail) {
    return (
      <InspectorBody>
        {loaded.error ? (
          <ErrorState title="Could not load this item" description={loaded.error} actions={<Button size="sm" onPress={loaded.reload}>Retry</Button>} />
        ) : (
          <LoadingState title="Loading…" />
        )}
      </InspectorBody>
    );
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
  const present = new Set(relations.map(({ r }) => relationDisplay(vocabulary, r.kind).family));
  const families = FAMILY_ORDER.filter((family) => present.has(family));
  const archived = entity.lifecycle === "archived";
  // Duplicate / Delete entity / Delete source file / Copy Reference / Copy Text (hide stays the view action above).
  const ops = graphActions({ entities: [entity], relations: [] }, { vocabulary, canWrite: actions.canWrite });
  return (
    <div className="xp-graph-inspector__item" data-testid="graph-entity-inspector" data-entity-id={entity.id}>
      <InspectorHeader
        type={<TypeLabel category={display.category} icon={display.icon} label={display.typeLabel} />}
        title={entity.label}
        meta={
          <>
            {entity.lifecycle !== "active" && <Badge dashed>{LIFECYCLE_LABEL[entity.lifecycle]}</Badge>}
            {hidden && <Badge dashed>Hidden from this view</Badge>}
            <span title="Who admitted it">Admitted by {originLabel(entity.origin)}</span>
            {entity.tombstoneReason && <span className="xp-graph-inspector__note">{entity.tombstoneReason}</span>}
            {display.kind.fileBacked && (
              <span className="xp-graph-inspector__note">
                Hide from view removes it from this view only. Delete Source {entity.kind === "folder" ? "Folder" : "File"} moves the file itself to{" "}
                {trashName()}; renaming is done in the Files explorer.
              </span>
            )}
          </>
        }
        actions={
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
                  runMenuKey(key, ops.actions, ops.extras);
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
                <MenuSeparator />
                {opMenuItems({ duplicate: ops.actions.duplicate, delete: ops.actions.delete, deletePermanently: ops.actions.deletePermanently }, ops.extras, { keys: false })}
              </Menu>
            </MenuTrigger>
          </Toolbar>
        }
      />
      <InspectorBody>
        <Section title="Source" count={detail.bindings.length}>
          <div className="xp-graph-stack">
            {detail.bindings.length ? (
              <ul className="xp-graph-rows">
                {detail.bindings.map((b, i) => (
                  <BindingRow key={`${b.path}-${i}`} binding={b} />
                ))}
              </ul>
            ) : (
              <p className="xp-muted">Not bound to a source.</p>
            )}
            {detail.coverage && (
              <p className="ui-meta" data-testid="graph-coverage">
                Extraction: {detail.coverage.state}
                {detail.coverage.extractor ? ` (${detail.coverage.extractor})` : ""}
                {detail.coverage.unresolved ? ` · ${detail.coverage.unresolved} unresolved` : ""}
                {detail.coverage.detail ? ` · ${detail.coverage.detail}` : ""}
              </p>
            )}
          </div>
        </Section>

        <Section title="Relationships" count={relations.length}>
          {relations.length === 0 && <p className="xp-muted">No relationships.</p>}
          {families.map((family) => (
            <div key={family} className="xp-graph-group" data-family={family}>
              <h4 className="ui-group-label">{FAMILY_LABELS[family]}</h4>
              <ul className="xp-graph-rows">
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

        <Section title="Evidence" count={detail.evidence.length}>
          {detail.evidence.length ? (
            <ul className="xp-graph-rows" data-divided>
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
          <KeyValueList
            items={[
              { key: "origin", label: "Admitted by", value: originLabel(entity.origin) },
              ...(entity.key ? [{ key: "key", label: "Key", value: entity.key, code: true }] : []),
              { key: "revision", label: "Item revision", value: String(entity.revision) },
              { key: "graph", label: "Graph revision", value: String(detail.graphRevision) },
              { key: "id", label: "Id", value: entity.id, code: true },
            ]}
          />
        </Section>
      </InspectorBody>
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
  const editable = relation.status !== "observed" && info.manual && actions.canWrite;
  const end = (id: string, label: string) => (
    <button type="button" className="xp-link" onClick={() => actions.onSelectEntity?.(id)}>
      {label} <ChevronRight size={14} aria-hidden />
    </button>
  );
  const facts: KeyValueItem[] = [
    { key: "from", label: "From", value: end(relation.from, from?.label ?? relation.from) },
    { key: "to", label: "To", value: end(relation.to, to?.label ?? relation.to) },
    { key: "rule", label: "Rule", value: info.rule },
    ...(relation.evidenceKind ? [{ key: "evidence", label: "Evidence kind", value: relation.evidenceKind }] : []),
    ...(relation.note ? [{ key: "note", label: "Note", value: relation.note }] : []),
  ];
  return (
    <div className="xp-graph-inspector__item" data-testid="graph-relation-inspector" data-relation-id={relation.id}>
      <InspectorHeader
        type={<TypeLabel category={RELATION_FAMILY_CATEGORY[info.family]} label={FAMILY_LABELS[info.family]} />}
        title={description}
        meta={
          <>
            <RelationStatus status={relation.status} form="text" />
            <span>Admitted by {originLabel(relation.origin)}</span>
          </>
        }
        actions={editable ? <RelationActions relation={relation} description={description} /> : undefined}
      />
      <InspectorBody>
        <Section title="Details">
          <div className="xp-graph-stack">
            <KeyValueList items={facts} />
            {relation.status === "observed" ? (
              <p className="xp-muted">Observed in the sources: to change it, change the source.</p>
            ) : !info.manual ? (
              <p className="xp-muted">This kind of relationship follows the files; it is not edited by hand.</p>
            ) : null}
          </div>
        </Section>
      </InspectorBody>
    </div>
  );
}

export function InspectorEmpty({ counts }: { counts?: string }) {
  return (
    <InspectorBody>
      <EmptyState title="Nothing selected" description={`Select an item or relationship to inspect it.${counts ? ` ${counts}` : ""}`} />
    </InspectorBody>
  );
}
