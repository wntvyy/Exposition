// Typed IPC for research in the project graph (research space, retained
// outcomes, experiments, reviewed promotion). Rust validates everything and
// owns identities; these wrappers only name commands and arguments.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type { CreateSpaceFileRequest } from "../../bindings/CreateSpaceFileRequest";
import type { EntityId } from "../../bindings/EntityId";
import type { ExperimentDetail } from "../../bindings/ExperimentDetail";
import type { ExperimentInput } from "../../bindings/ExperimentInput";
import type { ImportOutcome } from "../../bindings/ImportOutcome";
import type { ImportRequest } from "../../bindings/ImportRequest";
import type { OperationId } from "../../bindings/OperationId";
import type { PromotionOutcome } from "../../bindings/PromotionOutcome";
import type { PromotionPreview } from "../../bindings/PromotionPreview";
import type { PromotionRequest } from "../../bindings/PromotionRequest";
import type { PromotionsState } from "../../bindings/PromotionsState";
import type { ReconcileOutcome } from "../../bindings/ReconcileOutcome";
import type { ResearchExecuteRequest } from "../../bindings/ResearchExecuteRequest";
import type { ResearchListing } from "../../bindings/ResearchListing";
import type { ResearchSpaceInfo } from "../../bindings/ResearchSpaceInfo";
import type { RetainOutcome } from "../../bindings/RetainOutcome";
import type { RetainRequest } from "../../bindings/RetainRequest";
import type { RunSummary } from "../../bindings/RunSummary";
import type { SetResultStateRequest } from "../../bindings/SetResultStateRequest";
import type { SetupEvent } from "../../bindings/SetupEvent";
import type { SetupPlan } from "../../bindings/SetupPlan";
import type { SetupRequest } from "../../bindings/SetupRequest";
import type { SetupStatus } from "../../bindings/SetupStatus";
import type { SpaceFile } from "../../bindings/SpaceFile";

export const researchCommands = {
  /** The project's research space, or null when Research is not enabled. */
  space: () => invoke<ResearchSpaceInfo | null>("research_space"),
  /** Enable Research (one typed research space and its folders; runs nothing). */
  enable: () => invoke<ResearchSpaceInfo>("research_space_enable"),
  listing: () => invoke<ResearchListing>("research_listing"),
  createFile: (request: CreateSpaceFileRequest) => invoke<SpaceFile>("research_space_create_file", { request }),
  importFile: (request: ImportRequest) => invoke<ImportOutcome>("research_space_import", { request }),
  retain: (request: RetainRequest) => invoke<RetainOutcome>("research_retain", { request }),
  setResultState: (request: SetResultStateRequest) => invoke<RetainOutcome>("research_result_state", { request }),
  saveExperiment: (input: ExperimentInput) => invoke<ExperimentDetail>("research_experiment_save", { input }),
  experiment: (entity: EntityId) => invoke<ExperimentDetail>("research_experiment", { entity }),
  preparePromotion: (request: PromotionRequest) => invoke<PromotionPreview>("research_promotion_prepare", { request }),
  approvePromotion: (promotion: string) => invoke<PromotionOutcome>("research_promotion_approve", { promotion }),
  cancelPromotion: (promotion: string) => invoke<boolean>("research_promotion_cancel", { promotion }),
  promotions: () => invoke<PromotionsState>("research_promotions"),
  reconcilePromotion: (operation: OperationId) => invoke<ReconcileOutcome>("research_promotion_reconcile", { operation }),
  /** `research_execute` with declared inputs pinned in the run. */
  execute: (request: ResearchExecuteRequest) => invoke<RunSummary>("research_execute", { request }),
};

/**
 * Guided environment setup (project-local Python `.venv`, validated package
 * installs) and the environment remembered for the open project. Rust builds
 * every plan from discovered environments; starting re-plans and refuses a
 * plan that changed (`setup.planStale`).
 */
export const setupCommands = {
  /** Describe a setup for the open project (runs nothing). */
  plan: (request: SetupRequest) => invoke<SetupPlan>("research_setup_plan", { request }),
  /** Start a reviewed plan (the user's explicit action). Progress arrives as `research:setup` events. */
  start: (plan: SetupPlan) => invoke<SetupStatus>("research_setup_start", { plan }),
  /** Cancel a running setup; resolves false when it is not running. */
  cancel: (setupId: string) => invoke<boolean>("research_setup_cancel", { setupId }),
  /** The running setup of the open project, else its last finished one. */
  status: () => invoke<SetupStatus | null>("research_setup_status"),
  /** The environment remembered for the open project (may no longer be discovered). */
  selection: () => invoke<string | null>("research_selection_get"),
  /** Remember the environment chosen for the open project (`null` forgets it). */
  select: (environmentId: string | null) => invoke<void>("research_selection_set", { environmentId }),
};

export const setupEvents = {
  /** Setup status changes and step output, in order. */
  setup: (handler: (event: SetupEvent) => void): Promise<UnlistenFn> =>
    listen<SetupEvent>("research:setup", (e) => handler(e.payload)),
  /** The frontend fell behind; re-read the setup status. */
  resync: (handler: () => void): Promise<UnlistenFn> => listen("research:setup-resync", () => handler()),
};
