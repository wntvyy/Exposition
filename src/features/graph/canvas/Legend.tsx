// Canvas legend: what an edge's color (relationship family) and line style
// (assertion status) mean. Opened from the canvas controls; non-modal, so it
// can stay open while the graph is read. Samples use the edges' own stroke
// rules and arrowheads (canvas.css), so the legend cannot drift from them.
import type { AssertionStatus, RelationFamily } from "./types";

// Family names as in the inspector's relationship groups and the Filters
// popover; structure and resource use share the gray line.
const FAMILIES: { family: RelationFamily; label: string; detail?: string }[] = [
  { family: "math", label: "Mathematical dependencies", detail: "depends on, assumes, is a proof of" },
  { family: "citation", label: "References" },
  { family: "evidence", label: "Evidence and provenance" },
  { family: "formal", label: "Formal association" },
  { family: "structure", label: "Structure and resource use", detail: "contains, is declared in, imports" },
];

const STATUSES: { status: Exclude<AssertionStatus, "rejected">; label: string; detail: string }[] = [
  { status: "accepted", label: "Accepted", detail: "solid" },
  { status: "suggested", label: "Suggested", detail: "dashed" },
  { status: "observed", label: "Observed", detail: "dotted" },
];

function Sample({ markerPrefix, family, status }: { markerPrefix: string; family: RelationFamily; status: AssertionStatus }) {
  const marker = `${markerPrefix}-${family}${family === "citation" ? "-open" : ""}`;
  return (
    <svg className="xp-graph-legend__sample" viewBox="0 0 36 12" width={36} height={12} aria-hidden focusable="false">
      <path className="xp-graph-legend__line" data-family={family} data-status={status} d="M 2 6 L 33 6" markerEnd={`url(#${marker})`} />
    </svg>
  );
}

export function CanvasLegend({ id, markerPrefix }: { id: string; markerPrefix: string }) {
  return (
    <section id={id} className="xp-graph-legend" aria-label="Legend" data-testid="graph-legend">
      <h3 className="ui-group-label">Relationship family</h3>
      <ul className="xp-graph-legend__list">
        {FAMILIES.map(({ family, label, detail }) => (
          <li key={family} className="xp-graph-legend__row" data-family={family}>
            <Sample markerPrefix={markerPrefix} family={family} status="accepted" />
            <span>{label}</span>
            {detail && <span className="xp-graph-legend__detail">{detail}</span>}
          </li>
        ))}
      </ul>
      <h3 className="ui-group-label">Status</h3>
      <ul className="xp-graph-legend__list">
        {STATUSES.map(({ status, label, detail }) => (
          <li key={status} className="xp-graph-legend__row" data-status={status}>
            <Sample markerPrefix={markerPrefix} family="structure" status={status} />
            <span>{label}</span>
            <span className="xp-graph-legend__detail">{detail}</span>
          </li>
        ))}
      </ul>
    </section>
  );
}
