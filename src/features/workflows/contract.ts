// Workflow contract types used by the workflows feature (M9).
//
// The M9 contract (definitions v2: composites, conditions, repeats, max
// parallel; runtime: expanded plans, instances, ConditionNotMet, run cause;
// environment capture, manifests, reproduce; triggers) is implemented in Rust
// by lanes A–D and reaches TypeScript as generated `src/bindings`. Until those
// bindings exist, the shapes below follow M9_PLAN.md "Contracts" exactly
// (serde camelCase, `kind`-tagged enums, u64 as number). This file is the one
// switch point: once the generated types land, each extended type here becomes
// a re-export of its binding and the feature keeps importing from "./contract".
//
// New fields are optional here so that M8-shaped records (and older runs) are
// still read correctly; views treat a missing field as "not recorded".
// Landed so far: definitions (lane A), run cause (lane B), attempt environment (lane D), triggers (lane C).
import type { ParamValue } from "../../bindings/ParamValue";
import type { StepCondition } from "../../bindings/StepCondition";
import type { StepRepeat } from "../../bindings/StepRepeat";
import type { WorkflowPlanStep as WorkflowPlanStepBase } from "../../bindings/WorkflowPlanStep";
import type { WorkflowRun as WorkflowRunBase } from "../../bindings/WorkflowRun";
import type { WorkflowRunEvent as WorkflowRunEventBase } from "../../bindings/WorkflowRunEvent";
import type { WorkflowRunCause } from "../../bindings/WorkflowRunCause";
import type { WorkflowRunId } from "../../bindings/WorkflowRunId";
import type { WorkflowRunOutcome as WorkflowRunOutcomeBase } from "../../bindings/WorkflowRunOutcome";
import type { WorkflowRunPlan as WorkflowRunPlanBase } from "../../bindings/WorkflowRunPlan";
import type { WorkflowRunStep as WorkflowRunStepBase } from "../../bindings/WorkflowRunStep";
import type { WorkflowRunSummary as WorkflowRunSummaryBase } from "../../bindings/WorkflowRunSummary";
import type { WorkflowStepAttempt } from "../../bindings/WorkflowStepAttempt";
import type { WorkflowStepState as WorkflowStepStateBase } from "../../bindings/WorkflowStepState";

// ----- definitions (lane A) ------------------------------------------------------------------

export const WORKFLOW_SCHEMA_V1 = "exposition.workflow/1";
export const WORKFLOW_SCHEMA_V2 = "exposition.workflow/2";
/** The registered operation of a composite step (runs another saved workflow at a pinned revision). */
export const COMPOSITE_OPERATION = "workflow.composite";

// Landed from lane A (main ea61564): generated bindings. A v2 field is absent
// (never null) when unused, so a v1 definition stays byte-identical.
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

export type WorkflowConcurrencyLimits = {
  maxActiveRuns: number;
  maxInFlightSteps: number;
  restricted: number;
  native: number;
  agent: number;
  lean: number;
};

export type WorkflowPlanStep = WorkflowPlanStepBase & {
  /** Model requests reserved from the run's budget before the step is dispatched (0: none). */
  maxModelRequests?: number;
  /** Titles of the enclosing composites, outermost first (empty: a top-level step). */
  compositePath?: string[];
  condition?: StepCondition | null;
  repeat?: StepRepeat | null;
};

/** An environment difference between a recorded run and the plan reproducing it (lane D). */
export type WorkflowDriftNotice = {
  step: string | null;
  /** What differs: image, interpreter, toolchain, packages, model, os, app. */
  aspect: string;
  recorded: string | null;
  current: string | null;
  message: string;
};

export type WorkflowRunPlan = Omit<WorkflowRunPlanBase, "steps"> & {
  steps: WorkflowPlanStep[];
  maxParallel?: number;
  limits?: WorkflowConcurrencyLimits;
};

/** `workflow_run_reproduce_plan`: a normal plan pinned to a recorded run's inputs, with drift (lane D). */
export type WorkflowReproducePlan = { plan: WorkflowRunPlan; reproduces: WorkflowRunId; drift: WorkflowDriftNotice[] };

/** A plan as the review shows it: a reproduction carries what it reproduces and the drift. */
export type ReviewPlan = WorkflowRunPlan & { reproduces?: WorkflowRunId | null; drift?: WorkflowDriftNotice[] };

export type WorkflowStepState = WorkflowStepStateBase | "conditionNotMet";

export type { WorkflowEnvironment } from "../../bindings/WorkflowEnvironment";
export type { WorkflowStepAttempt };

export type WorkflowStepInstance = {
  /** 0-based position (forEach: the value's position; until: the iteration). */
  index: number;
  parameter: ParamValue | null;
  state: WorkflowStepState;
  attempts: WorkflowStepAttempt[];
  reason?: string | null;
};

export type WorkflowRunStep = Omit<WorkflowRunStepBase, "state" | "attempts"> & {
  state: WorkflowStepState;
  attempts: WorkflowStepAttempt[];
  instances?: WorkflowStepInstance[];
  /** Why it did not run (ConditionNotMet, Skipped), when the runner recorded one. */
  reason?: string | null;
};

export type WorkflowRunOutcome = WorkflowRunOutcomeBase & { reason?: string | null };

// Landed from lane B (main 33d7d53): generated bindings.
export type { ChangedInput } from "../../bindings/ChangedInput";
export type { WorkflowRunCause } from "../../bindings/WorkflowRunCause";

export type WorkflowRun = Omit<WorkflowRunBase, "steps" | "outcomes" | "plan"> & {
  steps: WorkflowRunStep[];
  outcomes: WorkflowRunOutcome[];
  plan: WorkflowRunPlan;
};

export type WorkflowRunSummary = WorkflowRunSummaryBase & { cause?: WorkflowRunCause };

export type WorkflowRunEvent = Omit<WorkflowRunEventBase, "stepState"> & { stepState: WorkflowStepState | null };

// ----- reproducibility (lane D) --------------------------------------------------------------

/** Where an exported run manifest was written (JSON). */
export type WorkflowManifestExport = { path: string; bytes: number; sha256: string };

// ----- triggers (lane C, main 56b8539): generated bindings ------------------------------------

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
