// Shared graph status displays: synchronization freshness and extraction
// coverage. Pending or unavailable synchronization is never shown as current.
// Wording matches the Agents page's graph context ("Graph synchronized" etc.).
import type { CoverageState } from "../../bindings/CoverageState";
import type { CoverageSummary } from "../../bindings/CoverageSummary";
import type { GraphFreshness } from "../../bindings/GraphFreshness";
import { Badge, StatusPill } from "../../ui/components";

/** The headline synchronization state of a surface (its one StatusPill). */
export function FreshnessPill({ freshness, size = "md" }: { freshness: GraphFreshness; size?: "sm" | "md" }) {
  const overlay = freshness.overlay.length;
  const pill = (() => {
    switch (freshness.state) {
      case "synchronized":
        return <StatusPill size={size} status="succeeded" label="Graph synchronized" />;
      case "reconciling":
        return <StatusPill size={size} status="running" label="Graph reconciling" detail={freshness.pending ? `${freshness.pending} files` : undefined} />;
      case "pending":
        // Same wording and treatment as the Agents page: answers may lag the sources (stale).
        return freshness.pending > 0 ? (
          <StatusPill size={size} status="stale" label="Reconciliation pending" detail={`${freshness.pending} files`} />
        ) : (
          <StatusPill size={size} status="stale" label={freshness.checkpointMs === null ? "Not yet reconciled" : "Reconciliation pending"} />
        );
      case "unavailable":
        return <StatusPill size={size} status="unavailable" label="Synchronization unavailable" />;
    }
  })();
  return (
    <span className="xp-graph-inline" data-testid="graph-freshness" data-state={freshness.state}>
      {pill}
      {overlay > 0 && (
        <Badge tone="info" dashed title="Unsaved editor buffers are included in answers and labelled as unsaved">
          {overlay} unsaved {overlay === 1 ? "buffer" : "buffers"}
        </Badge>
      )}
    </span>
  );
}

/** A file's extraction coverage state, worded the same in the Graph inspector and the Dependencies view. */
export function coverageStateLabel(state: CoverageState): string {
  switch (state) {
    case "complete":
      return "Complete";
    case "partial":
      return "Partial";
    case "pending":
      return "Pending";
    case "unsupported":
      return "Not supported";
    case "failed":
      return "Failed";
  }
}

/** One line on extraction coverage; unknown/partial coverage is stated, never implied complete. */
export function coverageSentence(coverage: CoverageSummary): string {
  if (coverage.resources === 0) return "No resources indexed yet.";
  const parts = [`${coverage.complete} of ${coverage.resources} resources fully extracted`];
  if (coverage.partial) parts.push(`${coverage.partial} partially`);
  if (coverage.pending) parts.push(`${coverage.pending} pending`);
  if (coverage.unsupported) parts.push(`${coverage.unsupported} unsupported`);
  if (coverage.failed) parts.push(`${coverage.failed} failed`);
  if (coverage.unresolvedBindings) parts.push(`${coverage.unresolvedBindings} unresolved bindings`);
  return `${parts.join(", ")}.`;
}
