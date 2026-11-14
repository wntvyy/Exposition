// The graph side of an agent task: the scoped graph context the agent was
// given (exactly as sent, consent filtering included), the project files it
// resolved to graph identities, and the graph changes it proposed.
import { Network } from "lucide-react";

import type { ContextEntity } from "../../bindings/ContextEntity";
import type { GraphContext } from "../../bindings/GraphContext";
import type { ProposalState } from "../../bindings/ProposalState";
import type { SyncState } from "../../bindings/SyncState";
import type { TaskGraphInfo } from "../../bindings/TaskGraphInfo";
import type { TaskProposal } from "../../bindings/TaskProposal";
import { Badge, Banner, DataTable, EmptyState, ListRow, Section, StatusText, type BadgeTone } from "../../ui/components";
import { relationDisplay } from "../graph/model";
import { RelationStatus } from "../graph/RelationStatus";
import { CONSENT_LABELS, type PillState } from "./format";

/** Graph synchronization in the shared status vocabulary. */
export function freshnessLabel(state: SyncState): PillState {
  switch (state) {
    case "synchronized":
      return { status: "succeeded", label: "Graph synchronized" };
    case "reconciling":
      return { status: "running", label: "Graph reconciling" };
    case "pending":
      return { status: "stale", label: "Reconciliation pending" };
    case "unavailable":
      return { status: "unavailable", label: "Graph unavailable" };
  }
}

/** A proposal's state with a specific label (never a generic "Done"). */
export function proposalLabel(state: ProposalState): PillState {
  switch (state) {
    case "awaitingApproval":
      return { status: "warning", label: "Waiting for approval" };
    case "applied":
      return { status: "succeeded", label: "Applied as suggestion" };
    case "declined":
      return { status: "canceled", label: "Declined" };
    case "conflict":
      return { status: "stale", label: "Conflict: not applied" };
    case "invalid":
      return { status: "failed", label: "Invalid: not applied" };
    case "failed":
      return { status: "failed", label: "Not applied" };
  }
}

const KIND_TITLES: Record<string, string> = {
  mathObject: "Mathematical object",
  formalDeclaration: "Formal declaration",
  externalResource: "External resource",
  researchSpace: "Research space",
};

export function kindTitle(kind: string): string {
  return KIND_TITLES[kind] ?? kind.charAt(0).toUpperCase() + kind.slice(1);
}

/** Short form of an opaque id for display (the full id is in the title). */
export function shortId(id: string): string {
  const dash = id.indexOf("-");
  return dash > 0 ? `${id.slice(0, dash + 1)}${id.slice(dash + 1, dash + 9)}` : id;
}

function entityName(entity: ContextEntity | undefined, id: string): string {
  if (!entity) return shortId(id);
  return entity.label ?? `${kindTitle(entity.kind)} ${shortId(entity.id)}`;
}

/** One-line summary for the task view (panel and page). */
export function graphSummary(context: GraphContext): string {
  const entities = context.entities.length;
  return `Graph context given at the start of the run: revision ${context.revision} · ${entities} ${entities === 1 ? "entity" : "entities"} in scope${
    context.truncated ? " (summarized)" : ""
  }`;
}

const EVIDENCE_KIND: Record<string, string> = {
  computational: "Computational",
  formalCheck: "Formal check",
  review: "Review",
  build: "Build",
  literature: "Literature",
};

/** Relation status tones shared with the Graph page (accepted solid; others dashed). */

/** Facts about an entity as badges (text always; dashed when provisional). */
function EntityBadges({ entity }: { entity: ContextEntity }) {
  const badges: { text: string; tone: BadgeTone; dashed?: boolean }[] = [];
  if (entity.binding && entity.binding !== "resolved") {
    badges.push({ text: `binding ${entity.binding}`, tone: entity.binding === "ambiguous" ? "warning" : "neutral", dashed: true });
  }
  if (entity.coverage) badges.push({ text: `coverage ${entity.coverage}`, tone: entity.coverage === "pending" ? "stale" : "neutral" });
  if (entity.lifecycle !== "active") badges.push({ text: entity.lifecycle, tone: "neutral", dashed: true });
  for (const e of entity.evidence) {
    // Current evidence is a fact, not a verification: neutral like the Graph page; stale/unknown never green.
    badges.push({
      text: `${EVIDENCE_KIND[e.kind] ?? e.kind} evidence: ${e.freshness}`,
      tone: e.freshness === "stale" ? "stale" : "neutral",
      dashed: e.freshness !== "current",
    });
  }
  if (badges.length === 0) return <span className="ui-meta">—</span>;
  return (
    <span className="agents-badges">
      {badges.map((b, i) =>
        // A settled fact reads as plain muted text; provisional and stale facts keep their outline badge.
        b.tone === "neutral" && !b.dashed ? (
          <span key={i} className="ui-meta">
            {b.text}
          </span>
        ) : (
          <Badge key={i} tone={b.tone} dashed={b.dashed}>
            {b.text}
          </Badge>
        ),
      )}
    </span>
  );
}

function ContextEntities({ context }: { context: GraphContext }) {
  return (
    <DataTable
      aria-label="Entities in the task's graph context"
      rows={context.entities}
      rowKey={(e) => e.id}
      rowProps={(e) => ({ "data-testid": "agents-graph-entity", "data-entity-id": e.id, "data-kind": e.kind })}
      columns={[
        { id: "kind", title: "Kind", width: "16%", sortValue: (e) => e.kind, render: (e) => kindTitle(e.kind) },
        {
          id: "name",
          title: "Name as given to the agent",
          isRowHeader: true,
          sortValue: (e) => e.label ?? "",
          render: (e) =>
            e.label ?? (
              <span className="ui-meta" data-testid="agents-graph-withheld">
                label withheld (metadata only)
              </span>
            ),
        },
        {
          id: "file",
          title: "File",
          width: "24%",
          sortValue: (e) => e.path ?? "",
          render: (e) => (e.path ? <code className="ui-code">{e.path}</code> : <span className="ui-meta">—</span>),
        },
        { id: "state", title: "State", width: "28%", render: (e) => <EntityBadges entity={e} /> },
      ]}
    />
  );
}

function ContextRelations({ context }: { context: GraphContext }) {
  const byId = new Map(context.entities.map((e) => [e.id, e]));
  if (context.relations.length === 0) return <p className="ui-meta">No relations among these entities.</p>;
  return (
    <ul className="agents-list" aria-label="Relations in the task's graph context">
      {context.relations.map((r) => (
        <li key={r.id} data-testid="agents-graph-relation" data-status={r.status}>
          <span>{entityName(byId.get(r.from), r.from)}</span>{" "}
          <span className="ui-meta" data-kind={r.kind}>
            {relationDisplay(null, r.kind).reading}
          </span>{" "}
          <span>{entityName(byId.get(r.to), r.to)}</span>{" "}
          <RelationStatus status={r.status} form="text" />{" "}
          <span className="ui-meta">
            by {r.origin}
            {r.note ? ` — ${r.note}` : ""}
          </span>
        </li>
      ))}
    </ul>
  );
}

/** What the context withheld or summarized: one info banner, however many notes apply. */
function ContextNotes({ context }: { context: GraphContext }) {
  const notes: { title: string; text: string }[] = [];
  if (context.consent === "metadataOnly") {
    notes.push({
      title: "Metadata only",
      text: "The agent received paths, kinds, versions, states, and counts. Labels, notes, and reasons derived from file content were withheld.",
    });
  }
  if (context.truncated) {
    notes.push({
      title: "Summarized",
      text: "More of the graph is in scope than the context lists; the agent can read more with graph_query.",
    });
  }
  const [first] = notes;
  if (!first) return null;
  if (notes.length === 1) {
    return (
      <Banner tone="info" title={first.title}>
        {first.text}
      </Banner>
    );
  }
  return (
    <Banner tone="info" title={notes.map((n) => n.title).join(" · ")}>
      <ul>
        {notes.map((n) => (
          <li key={n.title}>{n.text}</li>
        ))}
      </ul>
    </Banner>
  );
}

/** The graph context, resolved files, and their states. */
export function GraphContextView({ graph }: { graph: TaskGraphInfo }) {
  const context = graph.context;
  if (!context) {
    return (
      <EmptyState
        icon={Network}
        title="No graph context"
        description="This task has no recorded graph context (it was created before the project graph was attached to agent tasks)."
      />
    );
  }
  const fresh = freshnessLabel(context.freshness);
  const counts = context.counts.map((c) => `${c.count} ${kindTitle(c.kind).toLowerCase()}`).join(", ");
  const cov = context.coverage;
  return (
    <div className="agents-stack" data-testid="agents-graph-context" data-revision={context.revision} data-consent={context.consent}>
      <div className="agents-row ui-meta">
        <StatusText status={fresh.status} label={fresh.label} />
        <span>Graph revision {context.revision}</span>
        <code className="ui-code" title={context.project}>
          {shortId(context.project)}
        </code>
        {context.temporary && <StatusText status="warning" label="Temporary graph (read-only folder)" />}
      </div>
      <p className="ui-meta">
        Scope: {context.scope} · {CONSENT_LABELS[context.consent]} · {graph.contexts}{" "}
        {graph.contexts === 1 ? "context" : "contexts"} attached (one per run)
        {context.pending > 0 ? ` · ${context.pending} file(s) pending reconciliation` : ""}
      </p>
      <ContextNotes context={context} />
      <Section title="Entities" count={context.entities.length} collapsible defaultExpanded className="agents-section">
        <ContextEntities context={context} />
      </Section>
      <Section title="Relations" count={context.relations.length} collapsible defaultExpanded className="agents-section">
        <ContextRelations context={context} />
      </Section>
      <Section title="In scope" collapsible defaultExpanded={false} className="agents-section">
        <p>{counts || "No entities besides the project."}</p>
        <p className="ui-meta">
          Coverage: {cov.resources} files · {cov.complete} complete · {cov.partial} partial · {cov.unsupported} unsupported ·{" "}
          {cov.pending} pending · {cov.failed} failed · {cov.unresolvedBindings} unresolved bindings
        </p>
      </Section>
      <Section title="Authorized operations" collapsible defaultExpanded={false} className="agents-section">
        <ul className="agents-list" aria-label="Operations this task may perform">
          {context.operations.map((op) => (
            <li key={op.tool} data-testid="agents-graph-operation" data-allowed={op.allowed}>
              <code className="ui-code">{op.tool}</code>{" "}
              <span className={op.allowed ? undefined : "ui-meta"}>{op.detail}</span>
            </li>
          ))}
        </ul>
      </Section>
      <Section title="Files resolved to graph identities" count={graph.targets.length} collapsible defaultExpanded className="agents-section">
        {graph.targets.length === 0 ? (
          <p className="ui-meta">The task has not acted on any project file yet.</p>
        ) : (
          <ul className="agents-list" aria-label="Files the task acted on">
            {graph.targets.map((t) => (
              <li key={t.path} data-testid="agents-target" data-path={t.path} data-entity-id={t.entity ?? ""}>
                <code className="ui-code">{t.path}</code> <span>{t.uses.join(", ")}</span>{" "}
                <span className="ui-meta">
                  {t.entity ? `→ ${shortId(t.entity)}` : t.note ?? "no graph identity"}
                </span>
              </li>
            ))}
          </ul>
        )}
      </Section>
    </div>
  );
}

function Proposal({ proposal }: { proposal: TaskProposal }) {
  const pill = proposalLabel(proposal.state);
  return (
    <ListRow
      role="listitem"
      static
      className="agents-proposal"
      data-testid="agents-proposal"
      data-state={proposal.state}
      title={<StatusText status={pill.status} label={pill.label} />}
      meta={
        <span>
          based on graph revision {proposal.baseRevision}
          {proposal.revision !== null ? ` · applied at revision ${proposal.revision}` : ""}
        </span>
      }
    >
      <div className="agents-proposal__body">
        {proposal.summary && <p>{proposal.summary}</p>}
        <ul className="agents-list" aria-label="Effects">
          {proposal.effects.map((effect, i) => (
            <li key={i}>{effect}</li>
          ))}
        </ul>
        {proposal.diagnostics.length > 0 && (
          <ul className="agents-list" aria-label="Diagnostics">
            {proposal.diagnostics.map((d, i) => (
              <li key={i}>
                <code className="ui-code">{d.code}</code> {d.message}
              </li>
            ))}
          </ul>
        )}
        {proposal.message && <p className="ui-meta">{proposal.message}</p>}
        <details className="agents-disclosure">
          <summary>Operations (complete)</summary>
          <pre className="agents-pre">{JSON.stringify(proposal.operations, null, 2)}</pre>
        </details>
      </div>
    </ListRow>
  );
}

/** Graph changes the agent proposed in this task, newest first. */
export function ProposalList({ proposals }: { proposals: TaskProposal[] }) {
  if (proposals.length === 0) {
    return (
      <EmptyState
        icon={Network}
        title="No graph changes proposed"
        description="When the agent proposes entities or relations for the project graph, they appear here with their review state."
      />
    );
  }
  return (
    <div role="list" className="agents-rows" aria-label="Graph changes proposed by the agent" data-testid="agents-proposals">
      {[...proposals].reverse().map((p) => (
        <Proposal key={p.toolUseId} proposal={p} />
      ))}
    </div>
  );
}
