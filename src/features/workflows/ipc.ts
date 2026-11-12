// Typed IPC for workflow drafts. Rust owns the operation registry and the
// strict validation; nothing here runs a workflow (execution arrives in a
// later version). Never bypass these wrappers with raw `invoke`.
import { invoke } from "@tauri-apps/api/core";

import type { OperationContract } from "../../bindings/OperationContract";
import type { SavedWorkflow } from "../../bindings/SavedWorkflow";
import type { SaveWorkflowRequest } from "../../bindings/SaveWorkflowRequest";
import type { WorkflowDefinition } from "../../bindings/WorkflowDefinition";
import type { WorkflowId } from "../../bindings/WorkflowId";
import type { WorkflowPresetInfo } from "../../bindings/WorkflowPresetInfo";
import type { WorkflowPresetRequest } from "../../bindings/WorkflowPresetRequest";
import type { WorkflowSummary } from "../../bindings/WorkflowSummary";
import type { WorkflowValidation } from "../../bindings/WorkflowValidation";

export const workflowCommands = {
  /** The registered operation contracts (the only operations a step can name). */
  operations: () => invoke<OperationContract[]>("workflow_operations"),
  presets: () => invoke<WorkflowPresetInfo[]>("workflow_presets"),
  fromPreset: (request: WorkflowPresetRequest) => invoke<WorkflowDefinition>("workflow_from_preset", { request }),
  validate: (definition: WorkflowDefinition) => invoke<WorkflowValidation>("workflow_validate", { definition }),
  /** Save a new revision (incomplete drafts are saved with diagnostics; never runnable). */
  save: (request: SaveWorkflowRequest) => invoke<SavedWorkflow>("workflow_save", { request }),
  /** Import JSON text as a new draft (strict schema, fresh id, never run). */
  import: (text: string) => invoke<SavedWorkflow>("workflow_import", { text }),
  list: () => invoke<WorkflowSummary[]>("workflow_list"),
  get: (id: WorkflowId, revision: number | null = null) => invoke<SavedWorkflow>("workflow_get", { id, revision }),
};
