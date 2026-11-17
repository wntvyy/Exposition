// The application side of a project switch (see switch.ts for the order):
// what counts as unsaved and running work, the one decision dialog, saving,
// and closing discarded editors.
import { editors } from "../../app/editors";
import { commands } from "../../lib/ipc";
import { confirm } from "../../ui/components";
import { fileKey, openHandles, saveAll } from "../editor/documentClient";
import { workflowRunCommands } from "../workflows/ipc";
import {
  listNames,
  type ActiveWork,
  type DirtyItem,
  type SwitchChoice,
  type SwitchDecision,
  type SwitchDeps,
} from "./switch";

const baseName = (path: string) => path.split("/").pop() ?? path;

/** Unsaved workspace documents (also ones open only in Research) and other unsaved editors. */
export async function dirtyWork(): Promise<DirtyItem[]> {
  await Promise.all(openHandles().map((handle) => handle.sync.idle()));
  const items: DirtyItem[] = [];
  for (const doc of await commands.documentList()) {
    if (doc.path && doc.savedRevision !== doc.revision) {
      items.push({ key: fileKey(doc.path), name: baseName(doc.path), document: doc.id });
    }
  }
  for (const editor of editors.all()) {
    const key = editor.resource.key;
    if (!editor.dirty || key.startsWith("file:") || items.some((item) => item.key === key)) continue;
    items.push({ key, name: editor.resource.title, document: null });
  }
  return items;
}

/** Work of the open project that a switch stops: running workflow runs and research code. */
export async function activeWork(): Promise<ActiveWork[]> {
  const [runs, sessions] = await Promise.allSettled([workflowRunCommands.list(null), commands.researchSessions()]);
  const work: ActiveWork[] = [];
  if (runs.status === "fulfilled") {
    for (const run of runs.value) if (run.state === "running") work.push({ kind: "workflowRun", name: run.title });
  }
  if (sessions.status === "fulfilled") {
    for (const session of sessions.value) {
      const live = session.state.kind !== "detached" && session.state.kind !== "stopped" && session.state.kind !== "crashed";
      if (live && (session.activeRun !== null || session.queuedRuns.length > 0)) {
        work.push({ kind: "research", name: session.displayName });
      }
    }
  }
  return work;
}

const quoted = (names: string[]) => listNames(names.map((n) => `“${n}”`));

export function decisionTitle(decision: Pick<SwitchDecision, "action" | "from" | "to">): string {
  return `Close “${decision.from}” and ${decision.action === "open" ? "open" : "create"} “${decision.to}”`;
}

/** The one decision before a switch that would close unsaved work or stop running work. */
export function decideSwitch(decision: SwitchDecision): Promise<SwitchChoice> {
  const { dirty, active } = decision;
  const runs = active.filter((w) => w.kind === "workflowRun").map((w) => w.name);
  const research = active.filter((w) => w.kind === "research").map((w) => w.name);
  const verb = decision.action === "open" ? "Open" : "Create";
  const actions: { id: SwitchChoice; label: string; variant: "primary" | "secondary" }[] =
    dirty.length > 0
      ? [
          { id: "cancel", label: "Cancel", variant: "secondary" },
          { id: "discard", label: "Don't Save", variant: "secondary" },
          { id: "save", label: dirty.length === 1 ? "Save" : "Save All", variant: "primary" },
        ]
      : [
          { id: "cancel", label: "Cancel", variant: "secondary" },
          { id: "proceed", label: `Stop and ${verb}`, variant: "primary" },
        ];
  return confirm<SwitchChoice>({
    title: decisionTitle(decision),
    message: (
      <div className="xp-switch" data-testid="project-switch-body">
        {dirty.length > 0 && (
          <p data-testid="project-switch-unsaved">
            {listNames(dirty.map((d) => (d.document ? d.name : `the workflow draft “${d.name}”`)))}{" "}
            {dirty.length === 1 ? "has" : "have"} unsaved changes. They will be lost if you don't save them.
          </p>
        )}
        {runs.length > 0 && (
          <p data-testid="project-switch-runs">
            {runs.length === 1 ? `The workflow run ${quoted(runs)} is` : `The workflow runs ${quoted(runs)} are`} canceled
            and {runs.length === 1 ? "does" : "do"} not resume.
          </p>
        )}
        {research.length > 0 && (
          <p data-testid="project-switch-research">Research code running in {quoted(research)} stops.</p>
        )}
      </div>
    ),
    actions,
    cancelId: "cancel",
    // Saving is the safe default; stopping work is never preselected.
    defaultId: dirty.length > 0 ? "save" : "cancel",
    tone: "warning",
    size: "md",
    testId: "project-switch-decision",
  });
}

/** The real dependencies of `switchProject`. */
export function switchDeps(currentName: () => string | null): SwitchDeps {
  return {
    currentName,
    dirtyWork,
    activeWork,
    decide: decideSwitch,
    saveDocuments: saveAll,
    saveEditor: (key) => editors.save(key),
    discardEditors: async (keys) => {
      for (const key of keys) await editors.close(key, { discard: true });
    },
  };
}
