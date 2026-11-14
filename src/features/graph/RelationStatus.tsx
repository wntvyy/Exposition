// One presentation of a relationship's assertion status for every surface
// (Graph inspector and tables, Research inspector, Agents graph context):
// suggested is provisional (a dashed accent badge); accepted, observed, and
// rejected are settled facts (muted text). Canvas edges add the dash pattern.
import type { Relation } from "../../bindings/Relation";
import { Badge } from "../../ui/components";
import { STATUS_LABELS } from "./model";

export function relationStatusTitle(status: Relation["status"]): string {
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
 * A relationship's assertion status: a "· observed" suffix in a relationship
 * line (`inline`), or the plain word in a table cell or fact (`text`); a
 * suggestion is a dashed accent badge in both forms.
 */
export function RelationStatus({ status, form = "inline" }: { status: Relation["status"]; form?: "inline" | "text" }) {
  if (status === "suggested") {
    return (
      <Badge tone="accent" dashed title={relationStatusTitle(status)} data-status={status}>
        {STATUS_LABELS[status]}
      </Badge>
    );
  }
  return (
    <span className="ui-muted" title={relationStatusTitle(status)} data-status={status}>
      {form === "inline" ? `· ${STATUS_LABELS[status].toLowerCase()}` : STATUS_LABELS[status]}
    </span>
  );
}
