// Project permissions (Auto) and task allowances. Types come from Rust
// (`src/bindings`); Rust validates every request. Only the composer, the
// approval card, the task view, and Settings call these: rendered agent
// content has no IPC access. Kept in the agents feature (the shared
// `src/lib/ipc.ts` is owned by the integrator).
import { invoke } from "@tauri-apps/api/core";

import type { AllowanceKind } from "../../bindings/AllowanceKind";
import type { ApprovalScope } from "../../bindings/ApprovalScope";
import type { PermissionChoice } from "../../bindings/PermissionChoice";
import type { PermissionsChange } from "../../bindings/PermissionsChange";
import type { ProjectPermissionsEntry } from "../../bindings/ProjectPermissionsEntry";
import type { ProjectPermissionsState } from "../../bindings/ProjectPermissionsState";
import type { TaskId } from "../../bindings/TaskId";

export const agentPermissions = {
  /** The open project's permissions and its execution facts. */
  get: (root: string) => invoke<ProjectPermissionsState>("agents_permissions_get", { root }),
  /** Every project-permission record (Settings › Agents). */
  list: () => invoke<ProjectPermissionsEntry[]>("agents_permissions_list"),
  /** Grant or change the open project's permissions; a narrowing stops its Auto tasks. */
  set: (root: string, choice: PermissionChoice) => invoke<PermissionsChange>("agents_permissions_set", { root, choice }),
  /** Revoke a project's permissions (it need not be open); its Auto tasks stop. */
  revoke: (root: string, project: string | null) =>
    invoke<PermissionsChange>("agents_permissions_revoke", { root, project }),
  /** Answer an approval for this call only, or for the rest of the task. */
  resolveApproval: (task: TaskId, approval: string, approved: boolean, scope: ApprovalScope) =>
    invoke<void>("agents_approval_resolve", { task, approval, approved, scope }),
  /** Revoke one allowance of a task, or all of them (`null`). */
  revokeAllowance: (task: TaskId, kind: AllowanceKind | null) => invoke<void>("agents_allowance_revoke", { task, kind }),
};
