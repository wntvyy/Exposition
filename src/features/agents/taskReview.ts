// A task's review records for the conversation (turn summaries, "Needs you",
// the page's Changes action): read when the task has changed files, and
// again whenever its review changes. Quiet: a failed read leaves the
// summaries without saved/unsaved facts (the review view reports errors).
import { useEffect, useState } from "react";

import type { FileReview } from "../../bindings/FileReview";
import type { TaskId } from "../../bindings/TaskId";
import { commands } from "../../lib/ipc";
import { useAgents } from "./store";

const NONE: ReadonlyMap<string, FileReview> = new Map();

/** Review records by path (empty until read, or when the task changed nothing). */
export function useTaskReview(id: TaskId | null, changedFiles: number): ReadonlyMap<string, FileReview> {
  const version = useAgents((s) => (id ? (s.reviewVersions[id] ?? 0) : 0));
  const [state, setState] = useState<{ id: TaskId | null; files: ReadonlyMap<string, FileReview> }>({ id: null, files: NONE });
  useEffect(() => {
    if (!id || changedFiles === 0) return;
    let live = true;
    Promise.resolve()
      .then(() => commands.agentsReview(id))
      .then(
        (review) => {
          if (live && review) setState({ id, files: new Map(review.files.map((f) => [f.path, f])) });
        },
        () => {},
      );
    return () => {
      live = false;
    };
  }, [id, changedFiles, version]);
  return state.id === id && id !== null && changedFiles > 0 ? state.files : NONE;
}

/** Whether the open document holds unsaved changes (`FileReview.unsaved`; absent from older backends). */
export function isUnsaved(file: FileReview): boolean {
  return (file as FileReview & { unsaved?: boolean | null }).unsaved === true;
}

/** "3 · 2 unsaved" style counts for changed files. */
export function unsavedCount(files: ReadonlyMap<string, FileReview>, pendingOnly = false): number {
  let n = 0;
  for (const file of files.values()) if (isUnsaved(file) && (!pendingOnly || file.state === "pending")) n++;
  return n;
}
