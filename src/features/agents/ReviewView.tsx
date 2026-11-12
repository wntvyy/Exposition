import { ArrowLeft, Check, ExternalLink, Undo2 } from "lucide-react";
import { useEffect, useState } from "react";

import type { FileDiff } from "../../bindings/FileDiff";
import type { FileReview } from "../../bindings/FileReview";
import type { ReviewActionResult } from "../../bindings/ReviewActionResult";
import type { TaskId } from "../../bindings/TaskId";
import type { TaskReview } from "../../bindings/TaskReview";
import { editors } from "../../app/editors";
import { commands } from "../../lib/ipc";
import { Banner, Button, EmptyState, IconButton, LoadingState, StatusPill, type Status } from "../../ui/components";
import { DiffView } from "./DiffView";
import { isActive } from "./format";
import { agents, useAgents } from "./store";

export const REVIEW_PREFIX = "agents-review:";

const STATES: Record<FileReview["state"], { status: Status; label: string }> = {
  pending: { status: "warning", label: "Pending review" },
  accepted: { status: "succeeded", label: "Kept" },
  reverted: { status: "canceled", label: "Reverted" },
  undone: { status: "canceled", label: "Undone in editor" },
};

function FileRow(props: {
  file: FileReview;
  selected: boolean;
  disabled: boolean;
  onSelect: () => void;
  onAction: (action: "accept" | "revert") => void;
}) {
  const { file } = props;
  const state = STATES[file.state];
  const editedSince =
    file.state === "pending" && file.currentRevision !== null && file.tipRevision !== null && file.currentRevision > file.tipRevision;
  return (
    <li className="agents-review-file" data-selected={props.selected || undefined} data-testid="agents-review-file" data-path={file.path}>
      <button type="button" className="agents-review-file__name" aria-pressed={props.selected} onClick={props.onSelect}>
        <code className="agents-code">{file.path}</code>
        <span className="agents-muted agents-small">
          {file.kind === "created" ? "created by the agent" : `${file.changeCount} change${file.changeCount === 1 ? "" : "s"}`}
          {editedSince ? " · edited since" : ""}
        </span>
      </button>
      <span data-testid="agents-review-state" data-state={file.state}>
        <StatusPill status={state.status} label={state.label} size="sm" />
      </span>
      {file.entangled && <StatusPill status="failed" label="Overlapping edits" size="sm" />}
      <span className="agents-row">
        <Button size="sm" variant="ghost" icon={Check} isDisabled={props.disabled || file.state !== "pending"} onPress={() => props.onAction("accept")}>
          Keep
        </Button>
        <Button
          size="sm"
          variant="ghost"
          icon={Undo2}
          isDisabled={props.disabled || file.state === "reverted" || file.state === "undone"}
          onPress={() => props.onAction("revert")}
          data-testid="agents-revert-file"
        >
          Revert
        </Button>
      </span>
      {file.message && <p className="agents-muted agents-small agents-review-file__message">{file.message}</p>}
    </li>
  );
}

/** Per-file review of a task's changes: Monaco diff, keep, revert. */
export function ReviewView({ id, inEditor = false }: { id: TaskId; inEditor?: boolean }) {
  const version = useAgents((s) => s.reviewVersions[id] ?? 0);
  const task = useAgents((s) => s.details[id]?.summary ?? s.tasks.find((t) => t.id === id));
  const [review, setReview] = useState<TaskReview | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [diff, setDiff] = useState<FileDiff | null>(null);
  const [results, setResults] = useState<ReviewActionResult[]>([]);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let live = true;
    void agents.run(() => commands.agentsReview(id)).then((r) => {
      if (!live || !r) return;
      setReview(r);
      setSelected((current) => current ?? r.files[0]?.path ?? null);
    });
    return () => {
      live = false;
    };
  }, [id, version]);

  useEffect(() => {
    if (!selected) return setDiff(null);
    let live = true;
    void agents.run(() => commands.agentsReviewDiff(id, selected)).then((d) => {
      if (live && d) setDiff(d);
    });
    return () => {
      live = false;
    };
  }, [id, selected, version]);

  const running = task ? isActive(task.status) : false;

  async function act(action: "accept" | "revert", path: string | null) {
    setBusy(true);
    const out = await agents.run(() =>
      action === "accept" ? commands.agentsReviewAccept(id, path) : commands.agentsReviewRevert(id, path),
    );
    setBusy(false);
    if (out) setResults(out);
    const r = await agents.run(() => commands.agentsReview(id));
    if (r) setReview(r);
  }

  if (!review) return <LoadingState title="Loading changes…" />;
  const title = task?.title ?? id;

  return (
    <article className="agents-stack" data-in-editor={inEditor || undefined} aria-label={`Review: ${title}`} data-testid="agents-review">
      <div className="agents-heading">
        {!inEditor && <IconButton icon={ArrowLeft} size="sm" aria-label="Back to task" onPress={() => agents.show("task", id)} />}
        <h3 className="agents-heading__title">Review: {title}</h3>
      </div>
      <div className="agents-row">
        <Button size="sm" icon={Check} isDisabled={busy || running || review.files.length === 0} onPress={() => void act("accept", null)}>
          Keep all
        </Button>
        <Button
          size="sm"
          icon={Undo2}
          isDisabled={busy || running || review.files.length === 0}
          onPress={() => void act("revert", null)}
          data-testid="agents-revert-all"
        >
          Revert all
        </Button>
        {!inEditor && (
          <Button
            size="sm"
            variant="ghost"
            icon={ExternalLink}
            onPress={() => editors.open({ key: `${REVIEW_PREFIX}${id}`, title: `Review: ${title}` })}
            data-testid="agents-review-in-editor"
          >
            Open in editor area
          </Button>
        )}
      </div>
      {running && (
        <Banner tone="info" title="The task is still running">
          Cancel it before reverting; changes keep arriving while it runs.
        </Banner>
      )}
      {review.checkpoint && (
        <p className="agents-muted agents-small">
          Checkpoint recorded {new Date(review.checkpoint.createdAtMs).toLocaleString()} ({review.checkpoint.documents.length} open
          document{review.checkpoint.documents.length === 1 ? "" : "s"}). Changes apply to open documents and are not saved until
          you save them.
        </p>
      )}
      {results.length > 0 && (
        <ul className="agents-results" role="status" data-testid="agents-review-results">
          {results.map((r) => (
            <li key={r.path}>
              <StatusPill status={r.ok ? "succeeded" : "failed"} label={r.ok ? "Done" : "Refused"} size="sm" /> <code className="agents-code">{r.path}</code>:{" "}
              {r.message}
            </li>
          ))}
        </ul>
      )}
      {review.files.length === 0 ? (
        <EmptyState title="No changes" description="This task has not changed any files." fill={false} />
      ) : (
        <ul className="agents-review-files" aria-label="Changed files">
          {review.files.map((file) => (
            <FileRow
              key={file.path}
              file={file}
              selected={file.path === selected}
              disabled={busy || running}
              onSelect={() => setSelected(file.path)}
              onAction={(action) => void act(action, file.path)}
            />
          ))}
        </ul>
      )}
      {diff && (
        <section aria-label={`Diff of ${diff.path}`} className="agents-stack agents-diff-section">
          <p className="agents-muted agents-small">
            Left: {diff.path} without this task's changes · Right: after the task's latest change
          </p>
          <DiffView diff={diff} height={inEditor ? "fill" : 360} />
        </section>
      )}
    </article>
  );
}
