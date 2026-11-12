// Project generation from an objective (New Project › describe an objective).
//
// The New Project flow creates the destination folder, opens it (which
// attaches its empty project graph), and then calls `startProjectGeneration`.
// That starts an ordinary agent task in the new project: whole-project scope,
// process execution disabled unless the user chose otherwise, the user's
// permission and consent choices. The task receives the (near-empty) graph
// context like every task and builds files, research structure
// (`.exposition/research/main/…`), and graph associations only through the
// validated agent tools; it has no access outside the project folder and
// grants no trust. The task appears in the Agent view and on the Agents page.
import type { Budgets } from "../../bindings/Budgets";
import type { ContextConsent } from "../../bindings/ContextConsent";
import type { ExecutionMode } from "../../bindings/ExecutionMode";
import type { PermissionProfile } from "../../bindings/PermissionProfile";
import type { TaskSummary } from "../../bindings/TaskSummary";
import { agentGraph } from "./graphIpc";
import { agents } from "./store";

export interface ProjectGenerationOptions {
  /** The objective the user described. */
  objective: string;
  /** The new project's folder (already open in the workspace). */
  workspaceRoot: string;
  /** Default: each change waits for the user's approval. */
  permission?: PermissionProfile;
  /** Default: process execution disabled. */
  execution?: ExecutionMode;
  /** Default: the provider settings' default consent. */
  consent?: ContextConsent;
  budgets?: Budgets;
  /** Where to show the task: the Agents page (default) or the Agent view. */
  show?: "page" | "panel" | "none";
}

/** Start the generation task; resolves with the task (errors are thrown). */
export async function startProjectGeneration(options: ProjectGenerationOptions): Promise<TaskSummary> {
  // Subscribe the shared agent store (once); the task's events then reach
  // both the Agent view and the Agents page.
  void agents.init();
  const task = await agentGraph.createGenerationTask({
    objective: options.objective,
    workspaceRoot: options.workspaceRoot,
    permission: options.permission ?? "reviewEdits",
    execution: options.execution ?? null,
    consent: options.consent ?? null,
    budgets: options.budgets ?? null,
  });
  if ((options.show ?? "page") === "page") agents.showInPage(task.id, "run");
  else if (options.show === "panel") agents.show("task", task.id);
  return task;
}
