// One presentation of a relationship's assertion status for every surface
// (Graph inspector and tables, Research inspector, Agents graph context):
// suggested is provisional (a dashed accent badge); accepted, observed, and
// rejected are settled facts (muted text). Canvas edges add the dash pattern.
//
// An accepted formal link also carries the standing of its review: when the
// statement changed since it was reviewed, or can no longer be located, it
// reads "Needs re-review" (a warning that needs the user) instead of
// accepted; when the reviewed statement was never recorded, it says so.
import type { ReactNode } from "react";

import type { LinkReview } from "../../bindings/LinkReview";
import type { Relation } from "../../bindings/Relation";
import { Badge } from "../../ui/components";
import { STATUS_LABELS } from "./model";

export const NEEDS_RE_REVIEW_LABEL = "Needs re-review";
export const REVIEW_UNRECORDED_LABEL = "reviewed statement not recorded";

type Review = LinkReview | null | undefined;

/** The review no longer covers the statement: the link is not presented as reviewed. */
export function needsReReview(review: Review): boolean {
  return review?.state === "statementChanged" || review?.state === "statementMissing";
}

/** The review does not plainly stand (needs re-review, or what was reviewed is unknown). */
export function reviewQualified(relation: Pick<Relation, "status" | "review">): boolean {
  return relation.status === "accepted" && !!relation.review && relation.review.state !== "current";
}

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
 * suggestion is a dashed accent badge in both forms. Pass the relation's
 * `review` wherever a formal link can appear.
 */
export function RelationStatus({ status, review, form = "inline" }: { status: Relation["status"]; review?: Review; form?: "inline" | "text" }) {
  if (status === "suggested") {
    return (
      <Badge tone="accent" dashed title={relationStatusTitle(status)} data-status={status}>
        {STATUS_LABELS[status]}
      </Badge>
    );
  }
  if (status === "accepted" && review && needsReReview(review)) {
    return (
      <Badge tone="warning" title={review.reason ?? undefined} data-status={status} data-review={review.state}>
        {NEEDS_RE_REVIEW_LABEL}
      </Badge>
    );
  }
  const label = form === "inline" ? `· ${STATUS_LABELS[status].toLowerCase()}` : STATUS_LABELS[status];
  if (status === "accepted" && review?.state === "unrecorded") {
    return (
      <span className="ui-muted" title={review.reason ?? undefined} data-status={status} data-review={review.state}>
        {label} · {REVIEW_UNRECORDED_LABEL}
      </span>
    );
  }
  return (
    <span className="ui-muted" title={relationStatusTitle(status)} data-status={status} {...(review ? { "data-review": review.state } : {})}>
      {label}
    </span>
  );
}

/**
 * The main cell of a relationship row (`xp-graph-row`): its line, and under
 * it the reason when a formal link's review does not plainly stand.
 */
export function RelationLine({ review, children }: { review: Review; children: ReactNode }) {
  if (!review?.reason) return <p className="xp-graph-row__line xp-graph-row__main">{children}</p>;
  return (
    <div className="xp-graph-row__main">
      <p className="xp-graph-row__line">{children}</p>
      <RelationReviewNote review={review} />
    </div>
  );
}

/** Why a formal link's review does not plainly stand, as a visible line (nothing when it does). */
export function RelationReviewNote({ review }: { review: Review }) {
  if (!review?.reason) return null;
  return (
    <p className="ui-meta" data-testid="graph-link-review" data-review={review.state}>
      {review.reason}
    </p>
  );
}
