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
import { formatDateTime } from "../../lib/format";
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
  type BadgeTone,
  Button,
  confirm,
  EmptyState,
  ErrorState,
  FileIcon,
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
} from "../../ui/components";
import type { GraphCategory } from "../../ui/tokens";
import { runUserOperation } from "../operations/client";
import { targetKey, trashName } from "../operations/model";
import { opMenuItems, runMenuKey } from "../operations/OpMenu";
import { errorText, graphApi } from "./api";
import { runChange } from "./changes";
import { classifyEntity, graphActions } from "./graphOps";
import {
  CITED_EVIDENCE,
  entityDisplay,
  RELATION_FAMILY_CATEGORY,
  FAMILY_LABELS,
  FAMILY_ORDER,
  evidenceKindLabel,
  freshnessView,
  LIFECYCLE_LABEL,
  originLabel,
  relationDisplay,
  VERDICT_VIEW,
  type VocabularyIndex,
} from "./model";
import { RelationLine, RelationReviewNote, RelationStatus, reviewQualified } from "./RelationStatus";
import { revealSource } from "./reveal";
import { coverageStateLabel } from "./status";
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
    <span className="xp-graph-type ui-category-text" data-category={category}>
      {Icon && <Icon size={12} aria-hidden />}
      {label}
    </span>
  );
}

export { RelationStatus };

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
          <FileIcon name={binding.path} className="xp-graph-file-icon" />
          <span className="ui-code">{binding.path}</span> <span className="ui-muted">· {where}</span>
          {binding.state === "resolved" ? (
            <span className="ui-muted"> · resolved</span>
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
      <RelationLine review={relation.review}>
        {outgoing ? info.reading : info.inverse}{" "}
        {other && actions.onSelectEntity ? (
          <button type="button" className="xp-link" onClick={() => actions.onSelectEntity?.(other.id)}>
            {otherLabel}
          </button>
        ) : (
          <span>{otherLabel}</span>
        )}
        {other && <span className="ui-muted"> ({entityDisplay(vocabulary, other).typeLabel})</span>}{" "}
        <RelationStatus status={relation.status} review={relation.review} />
        {relation.status === "observed" && <span className="ui-muted"> from {originLabel(relation.origin)}</span>}
      </RelationLine>
      {canEdit && <RelationActions relation={relation} description={`${outgoing ? "" : otherLabel + " "}${info.reading}${outgoing ? " " + otherLabel : ""}`} />}
    </li>
  );
}

/**
 * Accept/Reject a suggestion; confirm or reject a formal link whose review
 * no longer plainly stands (confirming records the statement as it is now,
 * so it is unavailable while the statement cannot be located: the reason
 * line under the relationship says what to do first);
 * remove a relationship (semantic, confirmed).
 */
export function RelationActions({ relation, description }: { relation: Relation; description: string }) {
  return (
    <Toolbar aria-label="Relationship actions" className="xp-graph-row__actions">
      {reviewQualified(relation) && (
        <>
          <IconButton
            icon={Check}
            size="sm"
            aria-label="Confirm link"
            isDisabled={relation.review?.state === "statementMissing"}
            onPress={() => void runChange([{ op: "setRelationStatus", relation: relation.id, status: "accepted" }], "Confirm the link", "Link confirmed")}
          />
          <IconButton
            icon={X}
            size="sm"
            aria-label="Reject link"
            onPress={() => void runChange([{ op: "setRelationStatus", relation: relation.id, status: "rejected" }], "Reject the link", "Link rejected")}
          />
        </>
      )}
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

export { evidenceKindLabel };

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
    { key: "recorded", label: "Recorded", value: `${formatDateTime(Number(evidence.recordedAtMs), { seconds: true })} by ${originLabel(evidence.recordedBy)}` },
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
      {formal.dependencyFingerprints === null && <p className="ui-muted">Freshness against formal dependencies is unknown (no dependency fingerprints).</p>}
    </div>
  );
}

/**
 * Where an item's formal evidence is, when it is not on the item: Lean
 * evidence belongs to a formal declaration, never to the statement it
 * formalizes, so a statement points at its associated declarations (with the
 * state of each association) instead of reading as unchecked or as verified.
 * A declaration without evidence says how its verdict gets recorded.
 */
export function FormalEvidencePointer({
  detail,
  onSelectEntity,
}: {
  detail: EntityDetail;
  onSelectEntity?: (id: string) => void;
}) {
  if (detail.entity.kind === "formalDeclaration") {
    if (detail.evidence.length > 0) return null;
    return (
      <p className="ui-meta" data-testid="graph-evidence-how">
        A Lean check alone records nothing here. To record Lean&rsquo;s verdict for this declaration, open its file and choose
        Record evidence in the Lean panel.
      </p>
    );
  }
  const formalizations = detail.incoming.filter((r) => r.kind === "formalizes" && r.status !== "rejected");
  if (formalizations.length === 0) return null;
  const neighbors = new Map(detail.neighbors.map((n) => [n.id, n]));
  return (
    <div className="xp-graph-stack" data-testid="graph-evidence-formal">
      <p className="ui-meta">
        Lean evidence is recorded for a formal declaration, not for the item it formalizes. Associated with this item:
      </p>
      <ul className="xp-graph-rows">
        {formalizations.map((r) => {
          const label = neighbors.get(r.from)?.label ?? r.from;
          return (
            <li
              key={r.id}
              className="xp-graph-row"
              data-testid="graph-evidence-formal-declaration"
              data-entity-id={r.from}
              data-status={r.status}
              data-review={r.review?.state}
            >
              <RelationLine review={r.review}>
                {onSelectEntity ? (
                  <button type="button" className="xp-link" onClick={() => onSelectEntity(r.from)}>
                    {label}
                  </button>
                ) : (
                  label
                )}{" "}
                <RelationStatus status={r.status} review={r.review} />
              </RelationLine>
            </li>
          );
        })}
      </ul>
      <p className="ui-meta">
        Its evidence is on the declaration. An association records a correspondence; it is not a machine proof that the
        declaration and this item are equivalent.
      </p>
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
    <Section title="Change impact" count={report?.items.length}>
      <div className="xp-graph-stack" data-testid="graph-impact">
        <Button size="sm" icon={ListChecks} isPending={busy} onPress={() => void run()}>
          What needs review if this changes?
        </Button>
        {error && <p className="xp-inline-error">{error}</p>}
        {report && (
          <>
            {report.items.length === 0 ? (
              <p className="ui-muted">Nothing in the graph depends on it.</p>
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
                        <Badge tone={ACTION_LABEL[item.action].tone}>{ACTION_LABEL[item.action].label}</Badge>
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
            {report.items.some((item) => item.locatedIn) && (
              <p className="ui-meta" data-testid="graph-impact-file-scope">
                Items in a changed file are listed without what depends on them; ask this of an item to follow it.
              </p>
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
  // The note names Delete Source File/Folder only while the item still stands for an existing file or folder.
  const entityClass = classifyEntity(entity, vocabulary);
  const sourceFile = entityClass === "fileBacked" || entityClass === "folder";
  return (
    <div className="xp-graph-inspector__item" data-testid="graph-entity-inspector" data-entity-id={entity.id}>
      <InspectorHeader
        type={<TypeLabel category={display.category} icon={display.icon} label={display.typeLabel} />}
        title={entity.label}
        meta={
          <>
            {entity.lifecycle !== "active" && <Badge dashed>{LIFECYCLE_LABEL[entity.lifecycle]}</Badge>}
            {hidden && <Badge dashed>Hidden from this view</Badge>}
            <span>Admitted by {originLabel(entity.origin)}</span>
            {entity.tombstoneReason && <span className="xp-graph-inspector__note">{entity.tombstoneReason}</span>}
            {sourceFile && (
              <span className="xp-graph-inspector__note">
                Hide from view removes it from this view only. Delete Source {entity.kind === "folder" ? "Folder moves the folder" : "File moves the file"} itself to{" "}
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
            {actions.onRelate && (
              <Button size="sm" variant="ghost" icon={GitBranchPlus} isDisabled={!actions.canWrite} onPress={() => actions.onRelate?.(entity)}>
                Relate to…
              </Button>
            )}
            {/* Icon-only (accessible name and tooltip "Show neighborhood") so the actions keep one line at the inspector width. */}
            {actions.onFocus && <IconButton icon={Crosshair} size="sm" aria-label="Show neighborhood" onPress={() => actions.onFocus?.(entity.id)} />}
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
                  <MenuItem id="hide" icon={EyeOff} description="This view only; nothing is deleted">
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
              <p className="ui-muted">Not bound to a source.</p>
            )}
            {detail.coverage && (
              <p className="ui-meta" data-testid="graph-coverage">
                Extraction: {coverageStateLabel(detail.coverage.state)}
                {detail.coverage.extractor ? ` (${detail.coverage.extractor})` : ""}
                {detail.coverage.unresolved
                  ? ` · ${detail.coverage.unresolved} unresolved ${detail.coverage.unresolved === 1 ? "binding" : "bindings"}`
                  : ""}
                {detail.coverage.detail ? ` · ${detail.coverage.detail}` : ""}
              </p>
            )}
          </div>
        </Section>

        <Section title="Relationships" count={relations.length}>
          {relations.length === 0 && <p className="ui-muted">No relationships.</p>}
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
            <p className="ui-muted">No evidence recorded.</p>
          )}
          {detail.citedEvidence.length > 0 && (
            <div className="xp-graph-group" data-testid="graph-cited-evidence">
              <h4 className="ui-group-label">{CITED_EVIDENCE.title}</h4>
              <p className="ui-meta">{CITED_EVIDENCE.note}</p>
              <ul className="xp-graph-rows" data-divided>
                {detail.citedEvidence.map((ev) => (
                  <EvidenceRow key={ev.id} evidence={ev} freshness={freshnessOf(detail, ev.id)} />
                ))}
              </ul>
            </div>
          )}
          {/* The Mathematics view's own Formal verification section already lists the associations. */}
          {(entity.kind === "formalDeclaration" || !children) && (
            <FormalEvidencePointer detail={detail} onSelectEntity={actions.onSelectEntity} />
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
    ...(info.rule ? [{ key: "rule", label: "Rule", value: info.rule }] : []),
    ...(relation.evidenceKind ? [{ key: "evidence", label: "Evidence kind", value: evidenceKindLabel(relation.evidenceKind) }] : []),
    ...(relation.note ? [{ key: "note", label: "Note", value: relation.note }] : []),
  ];
  return (
    <div className="xp-graph-inspector__item" data-testid="graph-relation-inspector" data-relation-id={relation.id}>
      <InspectorHeader
        type={<TypeLabel category={RELATION_FAMILY_CATEGORY[info.family]} label={FAMILY_LABELS[info.family]} />}
        title={description}
        meta={
          <>
            <RelationStatus status={relation.status} review={relation.review} form="text" />
            <span>Admitted by {originLabel(relation.origin)}</span>
          </>
        }
        actions={editable ? <RelationActions relation={relation} description={description} /> : undefined}
      />
      <InspectorBody>
        <Section title="Details">
          <div className="xp-graph-stack">
            <KeyValueList items={facts} />
            <RelationReviewNote review={relation.review} />
            {relation.status === "observed" ? (
              <p className="ui-muted">Observed in the sources: to change it, change the source.</p>
            ) : !info.manual ? (
              <p className="ui-muted">This kind of relationship follows the files; it is not edited by hand.</p>
            ) : null}
          </div>
        </Section>
      </InspectorBody>
    </div>
  );
}

/** The inspector without one inspectable item: what is selected, if anything, and how to inspect. */
export function InspectorEmpty({ title = "Nothing selected", description }: { title?: string; description: string }) {
  return (
    <InspectorBody>
      <EmptyState title={title} description={description} />
    </InspectorBody>
  );
}

/**
 * The inspector's state for a selection it cannot show as one item: several
 * nodes, or an edge standing for relationships folded into a collapsed group.
 */
export function inspectorSelectionState(
  selection: { nodes: string[]; edge: string | null },
  edges: { id: string; count?: number }[],
): { title: string; description: string } | null {
  const folded = selection.edge ? edges.find((e) => e.id === selection.edge)?.count : undefined;
  if (folded !== undefined) {
    return folded === 1
      ? { title: "1 relationship in a collapsed group", description: "Expand the group to inspect it." }
      : { title: `${folded} relationships in a collapsed group`, description: "Expand the group to inspect them one by one." };
  }
  if (selection.nodes.length > 1) return { title: `${selection.nodes.length} items selected`, description: "Select one to inspect it." };
  return null;
}
