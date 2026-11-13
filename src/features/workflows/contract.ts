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
// Landed so far: definitions (lane A), run cause (lane B).
import type { ParamValue } from "../../bindings/ParamValue";
import type { StepCondition } from "../../bindings/StepCondition";
import type { StepRepeat } from "../../bindings/StepRepeat";
import type { WorkflowId } from "../../bindings/WorkflowId";
import type { WorkflowPlanStep as WorkflowPlanStepBase } from "../../bindings/WorkflowPlanStep";
import type { WorkflowRun as WorkflowRunBase } from "../../bindings/WorkflowRun";
import type { WorkflowRunEvent as WorkflowRunEventBase } from "../../bindings/WorkflowRunEvent";
import type { WorkflowRunCause } from "../../bindings/WorkflowRunCause";
import type { WorkflowRunId } from "../../bindings/WorkflowRunId";
import type { WorkflowRunOutcome as WorkflowRunOutcomeBase } from "../../bindings/WorkflowRunOutcome";
import type { WorkflowRunPlan as WorkflowRunPlanBase } from "../../bindings/WorkflowRunPlan";
import type { WorkflowRunStep as WorkflowRunStepBase } from "../../bindings/WorkflowRunStep";
import type { WorkflowRunSummary as WorkflowRunSummaryBase } from "../../bindings/WorkflowRunSummary";
import type { WorkflowStepAttempt as WorkflowStepAttemptBase } from "../../bindings/WorkflowStepAttempt";
import type { WorkflowStepState as WorkflowStepStateBase } from "../../bindings/WorkflowStepState";
import type { Containment } from "../../bindings/Containment";

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
  /** Titles of the enclosing composites, outermost first (empty: a top-level step). */
  compositePath?: string[];
  condition?: StepCondition | null;
  repeat?: StepRepeat | null;
};

/** An environment difference between a recorded run and its reproduction plan (lane D). */
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
  /** A reproduction plan: the run it reproduces (inputs pinned to that run's digests). */
  reproduces?: WorkflowRunId | null;
  drift?: WorkflowDriftNotice[];
};

export type WorkflowStepState = WorkflowStepStateBase | "conditionNotMet";

export type WorkflowEnvironment = {
  runner: string;
  containment: Containment;
  os: string;
  arch: string;
  appVersion: string;
  image: { name: string; id: string } | null;
  interpreter: { name: string; version: string } | null;
  packages: { name: string; version: string }[];
  packagesDigest: string | null;
  toolchain: { name: string; version: string; manifestDigest: string | null } | null;
  model: { provider: string; model: string } | null;
  seed: number | null;
  notes: string[];
};

export type WorkflowStepAttempt = WorkflowStepAttemptBase & { environment?: WorkflowEnvironment | null };

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

// ----- triggers (lane C) ---------------------------------------------------------------------

export type TriggerKind = { kind: "inputsChanged"; inputs: string[] } | { kind: "interval"; minutes: number };
export type TriggerState = { kind: "active" } | { kind: "suspended"; code: string; message: string } | { kind: "disabled" };
export type TriggerHistoryEntry = {
  atMs: number;
  /** fired | queued | coalesced | rateLimited | suspended | enabled | disabled */
  event: string;
  run: WorkflowRunId | null;
  message: string | null;
};

export type WorkflowTrigger = {
  id: string;
  workflow: WorkflowId;
  revision: number;
  kind: TriggerKind;
  debounceSeconds: number;
  maxRunsPerHour: number;
  maxRunsPerDay: number;
  allowNative: boolean;
  standingHash: string;
  state: TriggerState;
  createdAtMs: number;
  lastFiredMs: number | null;
  queued: boolean;
  history: TriggerHistoryEntry[];
};

export type TriggerSettings = {
  kind: TriggerKind;
  debounceSeconds: number;
  maxRunsPerHour: number;
  maxRunsPerDay: number;
  allowNative: boolean;
};

export type TriggerPlanRequest = { workflow: WorkflowId; revision: number } & TriggerSettings;

/** The standing plan a trigger is enabled with: the run plan plus what may change without asking. */
export type WorkflowStandingPlan = {
  plan: WorkflowRunPlan;
  standingHash: string;
  settings: TriggerSettings;
  /** Inputs whose saved content may change without a new approval. */
  watched: { input: string; label: string; path: string }[];
  /** Why this standing plan cannot be enabled (CodeSource watched, native without allowNative, ...). */
  blockers: { code: string; message: string }[];
};

export type EnableTriggerRequest = { plan: string; standingHash: string } & TriggerSettings;

export type WorkflowTriggerEvent = { trigger: string; workflow: WorkflowId; state: TriggerState };

/** Bounds from M9_PLAN acceptance 6 (Rust enforces them; the form only offers valid values). */
export const TRIGGER_BOUNDS = {
  debounceSeconds: { min: 2, max: 3600 },
  intervalMinutes: { min: 15, max: 7 * 24 * 60 },
  maxRunsPerHour: { min: 1, max: 12 },
  maxRunsPerDay: { min: 1, max: 100 },
};
