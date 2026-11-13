// Typed IPC for workflows. Rust owns the operation registry, the strict
// validation, run plans, approvals, and the runner; these wrappers only name
// commands, arguments, and events. Never bypass them with raw `invoke`.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type { OperationContract } from "../../bindings/OperationContract";
import type { PromotionPreview } from "../../bindings/PromotionPreview";
import type { SavedWorkflow } from "./contract";
import type { SaveWorkflowRequest } from "../../bindings/SaveWorkflowRequest";
import type { StartWorkflowRunRequest } from "../../bindings/StartWorkflowRunRequest";
import type { WorkflowAssociationDecision } from "../../bindings/WorkflowAssociationDecision";
import type { WorkflowDefinition } from "./contract";
import type { WorkflowId } from "../../bindings/WorkflowId";
import type { WorkflowPresetInfo } from "../../bindings/WorkflowPresetInfo";
import type { WorkflowPresetRequest } from "../../bindings/WorkflowPresetRequest";
import type { WorkflowRun } from "./contract";
import type { WorkflowRunEvent } from "./contract";
import type { WorkflowRunId } from "../../bindings/WorkflowRunId";
import type { WorkflowRunPlan } from "./contract";
import type { WorkflowRunSummary } from "./contract";
import type { WorkflowSummary } from "../../bindings/WorkflowSummary";
import type { WorkflowValidation } from "../../bindings/WorkflowValidation";
import type {
  EnableTriggerRequest,
  TriggerPlanRequest,
  WorkflowManifestExport,
  WorkflowReproducePlan,
  WorkflowStandingPlan,
  WorkflowTrigger,
  WorkflowTriggerEvent,
} from "./contract";

export const workflowCommands = {
  /** The registered operation contracts (the only operations a step can name), with runner availability. */
  operations: () => invoke<OperationContract[]>("workflow_operations"),
  presets: () => invoke<WorkflowPresetInfo[]>("workflow_presets"),
  fromPreset: (request: WorkflowPresetRequest) => invoke<WorkflowDefinition>("workflow_from_preset", { request }),
  validate: (definition: WorkflowDefinition) => invoke<WorkflowValidation>("workflow_validate", { definition }),
  /** Save a new revision (incomplete drafts are saved with diagnostics). Saving never runs anything. */
  save: (request: SaveWorkflowRequest) => invoke<SavedWorkflow>("workflow_save", { request }),
  /** Import JSON text as a new draft (strict schema, fresh id, never run). */
  import: (text: string) => invoke<SavedWorkflow>("workflow_import", { text }),
  list: () => invoke<WorkflowSummary[]>("workflow_list"),
  get: (id: WorkflowId, revision: number | null = null) => invoke<SavedWorkflow>("workflow_get", { id, revision }),
};

/**
 * Workflow runs. A run starts only from a plan the user reviewed in full:
 * `plan` resolves a saved revision; `start` sends that plan's id and approval
 * hash (refused with `workflow.planStale` when anything approved changed).
 */
export const workflowRunCommands = {
  plan: (id: WorkflowId, revision: number | null) => invoke<WorkflowRunPlan>("workflow_run_plan", { id, revision }),
  start: (request: StartWorkflowRunRequest) => invoke<WorkflowRun>("workflow_run_start", { request }),
  cancel: (run: WorkflowRunId) => invoke<WorkflowRun>("workflow_run_cancel", { run }),
  get: (run: WorkflowRunId) => invoke<WorkflowRun>("workflow_run_get", { run }),
  /** Runs of one workflow (or of every workflow, with `null`). */
  list: (workflow: WorkflowId | null) => invoke<WorkflowRunSummary[]>("workflow_runs", { workflow }),
  /** Mark an interrupted run as reviewed. Nothing is resumed or repeated. */
  acknowledge: (run: WorkflowRunId) => invoke<WorkflowRun>("workflow_run_acknowledge", { run }),
  decideAssociation: (decision: WorkflowAssociationDecision) => invoke<WorkflowRun>("workflow_association_decide", { decision }),
  /**
   * The prepared manuscript change behind a run's promotion proposal, shown
   * in full before approval (approve/cancel use the research promotion
   * commands by id). `null` once it was applied, discarded, or expired.
   */
  promotionPreview: (promotion: string) => invoke<PromotionPreview | null>("research_promotion_preview", { promotion }),
  /**
   * A new plan reproducing a recorded run: its revision with every input
   * pinned to the digests that run read, plus environment drift. It is
   * approved and started like any plan (`start`); the run records its cause
   * (reproduction of `run`).
   */
  reproducePlan: (run: WorkflowRunId) => invoke<WorkflowReproducePlan>("workflow_run_reproduce_plan", { run }),
  /** The run manifest as JSON text (revision, hashes, input/output digests, environments, usage, cause). */
  manifest: (run: WorkflowRunId) => invoke<string>("workflow_run_manifest", { run }),
  /** Write the run manifest (`destination` null: the default place in the project). */
  exportManifest: (run: WorkflowRunId, destination: string | null = null) =>
    invoke<WorkflowManifestExport>("workflow_run_manifest_export", { run, destination }),
};

/**
 * Explicit workflow triggers. Only the user enables one, by approving a
 * standing plan (`plan` → review → `enable` with its standing hash); any
 * change beyond the watched inputs' saved content suspends it. Triggers are
 * app-local and never offered to agents.
 */
export const workflowTriggerCommands = {
  list: (workflow: WorkflowId | null) => invoke<WorkflowTrigger[]>("workflow_trigger_list", { workflow }),
  plan: (request: TriggerPlanRequest) => invoke<WorkflowStandingPlan>("workflow_trigger_plan", { request }),
  enable: (request: EnableTriggerRequest) => invoke<WorkflowTrigger>("workflow_trigger_enable", { request }),
  disable: (trigger: string) => invoke<WorkflowTrigger>("workflow_trigger_disable", { trigger }),
};

export const workflowEvents = {
  /** A run or one of its steps changed: re-read that run. */
  run: (handler: (event: WorkflowRunEvent) => void): Promise<UnlistenFn> =>
    listen<WorkflowRunEvent>("workflow-run", (e) => handler(e.payload)),
  /** A trigger changed (fired, queued, suspended, enabled, disabled): re-read the triggers. */
  trigger: (handler: (event: WorkflowTriggerEvent) => void): Promise<UnlistenFn> =>
    listen<WorkflowTriggerEvent>("workflow-trigger", (e) => handler(e.payload)),
};
