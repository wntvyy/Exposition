// Agent commands that involve the project graph. Types come from Rust
// (`src/bindings`); Rust validates every request. Kept in the agents feature
// (the shared `src/lib/ipc.ts` is owned by the integrator).
import { invoke } from "@tauri-apps/api/core";

import type { EntityId } from "../../bindings/EntityId";
import type { GenerationTaskRequest } from "../../bindings/GenerationTaskRequest";
import type { GraphSnapshot } from "../../bindings/GraphSnapshot";
import type { TaskId } from "../../bindings/TaskId";
import type { TaskSummary } from "../../bindings/TaskSummary";

export const agentGraph = {
  /** Keep a stopped task as an investigation in the project graph (idempotent). */
  keepInvestigation: (task: TaskId) => invoke<EntityId>("agents_task_keep_investigation", { task }),

  /**
   * Start the project-generation task of a newly created project (New Project ›
   * describe an objective). The project must be open with its graph attached.
   */
  createGenerationTask: (request: GenerationTaskRequest) =>
    invoke<TaskSummary>("agents_task_create_generation", { request }),

  /** Saved investigations and workflow definitions of the open project. */
  keptEntities: () =>
    invoke<GraphSnapshot>("graph_snapshot", {
      query: { kinds: ["investigation", "workflow"], text: null, around: null, includeInactive: false, limit: 200 },
    }),
};
