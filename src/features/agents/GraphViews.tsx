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
import { Banner, EmptyState, Section, StatusPill } from "../../ui/components";
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

function ContextEntities({ context }: { context: GraphContext }) {
  return (
    <table className="agents-table" aria-label="Entities in the task's graph context">
      <thead>
        <tr>
          <th scope="col">Kind</th>
          <th scope="col">Name as given to the agent</th>
          <th scope="col">File</th>
          <th scope="col">State</th>
        </tr>
      </thead>
      <tbody>
        {context.entities.map((entity) => (
          <tr key={entity.id} data-testid="agents-graph-entity" data-entity-id={entity.id} data-kind={entity.kind}>
            <td>{kindTitle(entity.kind)}</td>
            <td title={entity.id}>
              {entity.label ?? (
                <span className="agents-muted" data-testid="agents-graph-withheld">
                  label withheld (metadata only)
                </span>
              )}
            </td>
            <td>{entity.path ? <code className="agents-code">{entity.path}</code> : <span className="agents-muted">—</span>}</td>
            <td className="agents-small">
              {[
                entity.binding ? `binding ${entity.binding}` : null,
                entity.coverage ? `coverage ${entity.coverage}` : null,
                entity.lifecycle !== "active" ? entity.lifecycle : null,
                entity.evidence.length
                  ? `evidence: ${entity.evidence.map((e) => `${e.kind} (${e.freshness})`).join(", ")}`
                  : null,
              ]
                .filter(Boolean)
                .join(" · ") || "—"}
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function ContextRelations({ context }: { context: GraphContext }) {
  const byId = new Map(context.entities.map((e) => [e.id, e]));
  if (context.relations.length === 0) return <p className="agents-muted agents-small">No relations among these entities.</p>;
  return (
    <ul className="agents-list" aria-label="Relations in the task's graph context">
      {context.relations.map((r) => (
        <li key={r.id} data-testid="agents-graph-relation" data-status={r.status}>
          <span>{entityName(byId.get(r.from), r.from)}</span> <code className="agents-code">{r.kind}</code>{" "}
          <span>{entityName(byId.get(r.to), r.to)}</span>{" "}
          <span className="agents-muted agents-small">
            ({r.status}, {r.origin}){r.note ? ` — ${r.note}` : ""}
          </span>
        </li>
      ))}
    </ul>
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
      <div className="agents-row">
        <StatusPill status={fresh.status} label={fresh.label} size="sm" />
        <span className="agents-small">Graph revision {context.revision}</span>
        <code className="agents-code agents-small" title={context.project}>
          {shortId(context.project)}
        </code>
        {context.temporary && <StatusPill status="warning" label="Temporary graph (read-only folder)" size="sm" />}
      </div>
      <p className="agents-small agents-muted">
        Scope: {context.scope} · {CONSENT_LABELS[context.consent]} · {graph.contexts}{" "}
        {graph.contexts === 1 ? "context" : "contexts"} attached (one per run)
        {context.pending > 0 ? ` · ${context.pending} file(s) pending reconciliation` : ""}
      </p>
      {context.consent === "metadataOnly" && (
        <Banner tone="info" title="Metadata only">
          The agent received paths, kinds, versions, states, and counts. Labels, notes, and reasons derived from file content
          were withheld.
        </Banner>
      )}
      {context.truncated && (
        <Banner tone="info" title="Summarized">
          More of the graph is in scope than the context lists; the agent can read more with graph_query.
        </Banner>
      )}
      <Section title={`Entities (${context.entities.length})`} collapsible defaultExpanded>
        <ContextEntities context={context} />
      </Section>
      <Section title={`Relations (${context.relations.length})`} collapsible defaultExpanded>
        <ContextRelations context={context} />
      </Section>
      <Section title="In scope" collapsible defaultExpanded={false}>
        <p className="agents-small">{counts || "No entities besides the project."}</p>
        <p className="agents-small agents-muted">
          Coverage: {cov.resources} files · {cov.complete} complete · {cov.partial} partial · {cov.unsupported} unsupported ·{" "}
          {cov.pending} pending · {cov.failed} failed · {cov.unresolvedBindings} unresolved bindings
        </p>
      </Section>
      <Section title="Authorized operations" collapsible defaultExpanded={false}>
        <ul className="agents-list" aria-label="Operations this task may perform">
          {context.operations.map((op) => (
            <li key={op.tool} data-testid="agents-graph-operation" data-allowed={op.allowed}>
              <code className="agents-code">{op.tool}</code>{" "}
              <span className={op.allowed ? undefined : "agents-muted"}>{op.detail}</span>
            </li>
          ))}
        </ul>
      </Section>
      <Section title={`Files resolved to graph identities (${graph.targets.length})`} collapsible defaultExpanded>
        {graph.targets.length === 0 ? (
          <p className="agents-muted agents-small">The task has not acted on any project file yet.</p>
        ) : (
          <ul className="agents-list" aria-label="Files the task acted on">
            {graph.targets.map((t) => (
              <li key={t.path} data-testid="agents-target" data-path={t.path} data-entity-id={t.entity ?? ""}>
                <code className="agents-code">{t.path}</code> <span className="agents-small">{t.uses.join(", ")}</span>{" "}
                <span className="agents-muted agents-small">
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
    <li className="agents-proposal agents-stack" data-testid="agents-proposal" data-state={proposal.state}>
      <div className="agents-row">
        <StatusPill status={pill.status} label={pill.label} size="sm" />
        <span className="agents-small agents-muted">
          based on graph revision {proposal.baseRevision}
          {proposal.revision !== null ? ` · applied at revision ${proposal.revision}` : ""}
        </span>
      </div>
      <ul className="agents-list" aria-label="Effects">
        {proposal.effects.map((effect, i) => (
          <li key={i}>{effect}</li>
        ))}
      </ul>
      {proposal.diagnostics.length > 0 && (
        <ul className="agents-list" aria-label="Diagnostics">
          {proposal.diagnostics.map((d, i) => (
            <li key={i} className="agents-small">
              <code className="agents-code">{d.code}</code> {d.message}
            </li>
          ))}
        </ul>
      )}
      {proposal.message && <p className="agents-small agents-muted">{proposal.message}</p>}
      <details className="agents-disclosure">
        <summary>Operations (complete)</summary>
        <pre className="agents-pre">{JSON.stringify(proposal.operations, null, 2)}</pre>
      </details>
    </li>
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
    <ul className="agents-proposals" aria-label="Graph changes proposed by the agent" data-testid="agents-proposals">
      {[...proposals].reverse().map((p) => (
        <Proposal key={p.toolUseId} proposal={p} />
      ))}
    </ul>
  );
}
