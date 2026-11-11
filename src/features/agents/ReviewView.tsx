import { useEffect, useState } from "react";

import type { FileDiff } from "../../bindings/FileDiff";
import type { FileReview } from "../../bindings/FileReview";
import type { ReviewActionResult } from "../../bindings/ReviewActionResult";
import type { TaskId } from "../../bindings/TaskId";
import type { TaskReview } from "../../bindings/TaskReview";
import { commands } from "../../lib/ipc";
import { DiffView } from "./DiffView";
import { isActive } from "./format";
import { agents, useAgents } from "./store";

const STATE_LABELS = { pending: "Pending review", accepted: "Kept", reverted: "Reverted" } as const;

function FileRow(props: {
  file: FileReview;
  selected: boolean;
  disabled: boolean;
  onSelect: () => void;
  onAction: (action: "accept" | "revert") => void;
}) {
  const { file } = props;
  const tone = file.state === "pending" ? "warning" : file.state === "accepted" ? "success" : "neutral";
  return (
    <li className={`agents-review-file${props.selected ? " is-selected" : ""}`} data-testid="agents-review-file" data-path={file.path}>
      <button type="button" className="agents-review-file-name" aria-pressed={props.selected} onClick={props.onSelect}>
        <code>{file.path}</code>
        <span className="agents-muted">
          {file.kind === "created" ? "created" : `${file.changeCount} change${file.changeCount === 1 ? "" : "s"}`}
        </span>
      </button>
      <span className={`agents-badge agents-tone-${tone}`}>{STATE_LABELS[file.state]}</span>
      {file.entangled && <span className="agents-badge agents-tone-danger">Overlapping edits</span>}
      {file.currentRevision !== null && file.tipRevision !== null && file.currentRevision > file.tipRevision && file.state === "pending" && (
        <span className="agents-muted">edited since</span>
      )}
      <span className="agents-actions">
        <button
          type="button"
          className="agents-button"
          disabled={props.disabled || file.state !== "pending"}
          onClick={() => props.onAction("accept")}
        >
          Keep
        </button>
        <button
          type="button"
          className="agents-button"
          disabled={props.disabled || file.state === "reverted"}
          onClick={() => props.onAction("revert")}
          data-testid="agents-revert-file"
        >
          Revert
        </button>
      </span>
      {file.message && <p className="agents-muted agents-review-message">{file.message}</p>}
    </li>
  );
}

export function ReviewView({ id }: { id: TaskId }) {
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

  return (
    <article className="agents-review" aria-label="Review changes" data-testid="agents-review">
      <header className="agents-task-header">
        <h3 className="agents-task-heading">Review: {task?.title ?? id}</h3>
        <div className="agents-header-actions">
          <button type="button" className="agents-button" onClick={() => agents.show("task", id)}>
            Back to task
          </button>
          <button type="button" className="agents-button" disabled={busy || running} onClick={() => void act("accept", null)}>
            Keep all
          </button>
          <button
            type="button"
            className="agents-button agents-button-danger"
            disabled={busy || running}
            onClick={() => void act("revert", null)}
            data-testid="agents-revert-all"
          >
            Revert all
          </button>
        </div>
      </header>
      {running && <p className="agents-hint">The task is still running; cancel it before reverting.</p>}
      {review?.checkpoint && (
        <p className="agents-muted">
          Checkpoint recorded {new Date(review.checkpoint.createdAtMs).toLocaleString()} with{" "}
          {review.checkpoint.documents.length} open document{review.checkpoint.documents.length === 1 ? "" : "s"}.
          Changes are applied to open documents and are not saved until you save them.
        </p>
      )}
      {results.length > 0 && (
        <ul className="agents-results" role="status" data-testid="agents-review-results">
          {results.map((r) => (
            <li key={r.path} className={r.ok ? "agents-tone-success" : "agents-tone-danger"}>
              <code>{r.path}</code>: {r.message}
            </li>
          ))}
        </ul>
      )}
      {review && review.files.length === 0 && <p className="agents-muted">This task has not changed any files.</p>}
      <ul className="agents-review-files" aria-label="Changed files">
        {review?.files.map((file) => (
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
      {diff && (
        <section aria-label={`Diff of ${diff.path}`} className="agents-diff-section">
          <p className="agents-muted agents-diff-legend">
            Left: {diff.path} without this task's changes · Right: after the task's latest change
          </p>
          <DiffView diff={diff} />
        </section>
      )}
    </article>
  );
}
