// Workflow contract types used by the workflows feature (M9).
//
// The M9 contract (definitions v2: composites, conditions, repeats, max
// parallel; runtime: expanded plans, instances, ConditionNotMet, run cause;
// environment capture, manifests, reproduce; triggers) is implemented in Rust
// by lanes A–D and reaches TypeScript as generated `src/bindings`. The feature
// imports these types from here (one switch point): everything below is a
// re-export of its generated binding, except one view-level addition:
// - `ReviewPlan`: a plan as the review dialog shows it (a reproduction carries
//   what it reproduces and its drift, from `WorkflowReproducePlan`).

import type { WorkflowEnvironmentDrift } from "../../bindings/WorkflowEnvironmentDrift";
import type { WorkflowRunId } from "../../bindings/WorkflowRunId";
import type { WorkflowRunPlan } from "../../bindings/WorkflowRunPlan";

// ----- definitions (lane A) ------------------------------------------------------------------

export const WORKFLOW_SCHEMA_V1 = "exposition.workflow/1";
export const WORKFLOW_SCHEMA_V2 = "exposition.workflow/2";
/** The registered operation of a composite step (runs another saved workflow at a pinned revision). */
export const COMPOSITE_OPERATION = "workflow.composite";

// A v2 field is absent (never null) when unused, so a v1 definition stays byte-identical.
export type { CompareOp } from "../../bindings/CompareOp";
export type { CompositeRef } from "../../bindings/CompositeRef";
export type { ConditionTest } from "../../bindings/ConditionTest";
export type { ConditionValue } from "../../bindings/ConditionValue";
export type { Exhausted } from "../../bindings/Exhausted";
export type { SavedWorkflow } from "../../bindings/SavedWorkflow";
export type { StepCondition } from "../../bindings/StepCondition";
export type { StepRepeat } from "../../bindings/StepRepeat";
export type { WorkflowDefinition } from "../../bindings/WorkflowDefinition";
export type { WorkflowExecutionOptions } from "../../bindings/WorkflowExecutionOptions";
export type { WorkflowStep } from "../../bindings/WorkflowStep";

// ----- runtime (lane B) ----------------------------------------------------------------------

export type { ChangedInput } from "../../bindings/ChangedInput";
export type { WorkflowConcurrencyLimits } from "../../bindings/WorkflowConcurrencyLimits";
export type { WorkflowPlanStep } from "../../bindings/WorkflowPlanStep";
export type { WorkflowRun } from "../../bindings/WorkflowRun";
export type { WorkflowRunCause } from "../../bindings/WorkflowRunCause";
export type { WorkflowRunEvent } from "../../bindings/WorkflowRunEvent";
export type { WorkflowRunOutcome } from "../../bindings/WorkflowRunOutcome";
export type { WorkflowRunPlan };
export type { WorkflowRunStep } from "../../bindings/WorkflowRunStep";
export type { WorkflowStepInstance } from "../../bindings/WorkflowStepInstance";
export type { WorkflowStepState } from "../../bindings/WorkflowStepState";

export type { WorkflowRunSummary } from "../../bindings/WorkflowRunSummary";

// ----- environment, manifests, reproduce (lane D) --------------------------------------------

export type { WorkflowEnvironment } from "../../bindings/WorkflowEnvironment";
export type { WorkflowEnvironmentDrift as WorkflowDriftNotice } from "../../bindings/WorkflowEnvironmentDrift";
export type { WorkflowManifestExport } from "../../bindings/WorkflowManifestExport";
export type { WorkflowReproducePlan } from "../../bindings/WorkflowReproducePlan";
export type { WorkflowStepAttempt } from "../../bindings/WorkflowStepAttempt";

/** A plan as the review shows it: a reproduction carries what it reproduces and the drift. */
export type ReviewPlan = WorkflowRunPlan & { reproduces?: WorkflowRunId | null; drift?: WorkflowEnvironmentDrift[] };

// ----- triggers (lane C) ---------------------------------------------------------------------

export type { EnableWorkflowTriggerRequest } from "../../bindings/EnableWorkflowTriggerRequest";
export type { TriggerHistoryEntry } from "../../bindings/TriggerHistoryEntry";
export type { TriggerHistoryEvent } from "../../bindings/TriggerHistoryEvent";
export type { TriggerKind } from "../../bindings/TriggerKind";
export type { TriggerState } from "../../bindings/TriggerState";
export type { TriggerWatchedInput } from "../../bindings/TriggerWatchedInput";
export type { WorkflowTrigger } from "../../bindings/WorkflowTrigger";
export type { WorkflowTriggerEvent } from "../../bindings/WorkflowTriggerEvent";
export type { WorkflowTriggerId } from "../../bindings/WorkflowTriggerId";
export type { WorkflowTriggerLimits } from "../../bindings/WorkflowTriggerLimits";
export type { WorkflowTriggerPlan } from "../../bindings/WorkflowTriggerPlan";
export type { WorkflowTriggerPlanRequest } from "../../bindings/WorkflowTriggerPlanRequest";
export type { WorkflowTriggerSettings } from "../../bindings/WorkflowTriggerSettings";
