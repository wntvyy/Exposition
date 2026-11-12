// Typed IPC for project creation. Types come from Rust (`src/bindings`). Rust
// parses and validates every blueprint and destination; the webview only
// previews and requests. Never bypass these wrappers with raw `invoke`.
import { invoke } from "@tauri-apps/api/core";

import type { BlueprintValidation } from "../../bindings/BlueprintValidation";
import type { CapabilityReport } from "../../bindings/CapabilityReport";
import type { CreateProjectRequest } from "../../bindings/CreateProjectRequest";
import type { CreationAction } from "../../bindings/CreationAction";
import type { CreationResolution } from "../../bindings/CreationResolution";
import type { CreationStatus } from "../../bindings/CreationStatus";
import type { DestinationCheck } from "../../bindings/DestinationCheck";
import type { OperationId } from "../../bindings/OperationId";
import type { PresetCatalog } from "../../bindings/PresetCatalog";
import type { ProjectBlueprint } from "../../bindings/ProjectBlueprint";
import type { ProjectCreated } from "../../bindings/ProjectCreated";
import type { ProjectPresetRequest } from "../../bindings/ProjectPresetRequest";

export const projectCommands = {
  /** The presets (immediate). */
  presets: () => invoke<PresetCatalog>("project_presets"),
  /** Availability of declared tools (may wait for first-time discovery). */
  capabilities: () => invoke<CapabilityReport>("project_capabilities"),
  /** A preset's blueprint with the chosen options (fresh ids each time). */
  presetBlueprint: (request: ProjectPresetRequest) => invoke<ProjectBlueprint>("project_preset_blueprint", { request }),
  /** Strict validation of blueprint JSON text, with a summary for preview. */
  validate: (blueprint: string) => invoke<BlueprintValidation>("project_blueprint_validate", { blueprint }),
  /** Whether a folder can hold a new project (new or empty, not too broad). */
  checkDestination: (destination: string) => invoke<DestinationCheck>("project_check_destination", { destination }),
  /** Create, open, and record the project (graph included). */
  create: (request: CreateProjectRequest) => invoke<ProjectCreated>("project_create", { request }),
  /** Unfinished (interrupted) creations of the open project. */
  creationStatus: () => invoke<CreationStatus[]>("project_creation_status"),
  creationResolve: (operation: OperationId, action: CreationAction) =>
    invoke<CreationResolution>("project_creation_resolve", { operation, action }),
};
