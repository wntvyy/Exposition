// Typed IPC for research in the project graph (research space, retained
// outcomes, experiments, reviewed promotion). Rust validates everything and
// owns identities; these wrappers only name commands and arguments.
import { invoke } from "@tauri-apps/api/core";

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
