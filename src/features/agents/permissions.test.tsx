import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ApprovalRequest } from "../../bindings/ApprovalRequest";
import type { ProjectPermissionsState } from "../../bindings/ProjectPermissionsState";
import type { TaskSpec } from "../../bindings/TaskSpec";

const core = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => core);

const ipc = vi.hoisted(() => ({
  agentsApprovalResolve: vi.fn(),
  agentsTaskCreate: vi.fn(),
  agentsTaskDetail: vi.fn(),
}));
vi.mock("../../lib/ipc", () => ({
  commands: ipc,
  events: { agents: vi.fn(), agentsResync: vi.fn() },
  errorMessage: (e: unknown) => String(e),
}));

const { authorizationLabel, environmentText, forTaskSentence, profileSummary, specLabels, toolSummary } = await import("./format");
const { ApprovalCard } = await import("./ApprovalCard");
const { TaskComposer, DEFAULT_BUDGETS } = await import("./TaskComposer");
const { TaskAllowances } = await import("./permissions");
const { initialAgentsState, permissionsNotification, reduceEvent, useAgents } = await import("./store");

const spec: TaskSpec = {
  prompt: "p",
  workspaceRoot: "C:/paper",
  scope: { kind: "workspace" },
  permission: "autoApply",
  execution: "approveEach",
  consent: "scopeContent",
  budgets: DEFAULT_BUDGETS,
  provider: { provider: "scripted", model: "m", effort: "low" },
  approvalMode: "auto",
};

describe("permission wording", () => {
  it("names how each call was authorized, quietly", () => {
    expect(authorizationLabel(null)).toBeNull();
    expect(authorizationLabel({ kind: "notRequired" })).toBeNull();
    expect(authorizationLabel({ kind: "taskSettings" })).toBe("Allowed by task settings");
    expect(authorizationLabel({ kind: "projectPermissions", version: 3 })).toBe("Auto: project permissions");
    expect(authorizationLabel({ kind: "taskAllowance", kinds: ["nativeCode"] })).toBe("Allowed for this task");
    expect(authorizationLabel({ kind: "approved", forTask: false })).toBe("Approved by you");
    expect(authorizationLabel({ kind: "approved", forTask: true })).toBe("Approved by you for this task");
  });

  it("describes Auto tasks by their mode and any ceiling, Ask tasks by their settings", () => {
    expect(specLabels(spec)).toEqual(["Auto: project permissions", "File contents in scope"]);
    expect(specLabels({ ...spec, permission: "readOnly", execution: "disabled" })).toEqual([
      "Auto: project permissions",
      "Read only",
      "No commands",
      "File contents in scope",
    ]);
    expect(specLabels({ ...spec, approvalMode: "ask", permission: "reviewEdits", execution: "disabled" })).toEqual([
      "Ask before each change",
      "No commands",
      "File contents in scope",
    ]);
  });

  it("states the execution environment as enforced, never calling native a sandbox", () => {
    expect(profileSummary({ files: "edit", code: "restricted", setup: false })).toBe(
      "Files: edit · Code: restricted container · Setup: off",
    );
    const native = environmentText("native", { trusted: false, restrictedAvailable: true });
    expect(native[0]!.text).toContain("your account's privileges — not a sandbox");
    expect(native[0]!.text).toContain("cannot confirm each action inside running code");
    expect(native[1]!.text).toContain("not trusted");
    const restricted = environmentText("restricted", { trusted: true, restrictedAvailable: true });
    expect(restricted[0]!.text).toContain("no network, read-only project, non-root, no host environment; never falls back");
    expect(environmentText("restricted", { trusted: true, restrictedAvailable: false })[0]!.text).toContain(
      "never run natively instead",
    );
    expect(toolSummary("install_packages", { packages: ["numpy", "pandas>=2.2"] })).toBe("numpy pandas>=2.2");
    expect(forTaskSentence(["nativeCode", "documentChanges"])).toBe(
      "Approve for this task also covers later native code and file changes in scope in this task; revoke it from the task.",
    );
  });
});

const approval: ApprovalRequest = {
  id: "approval-1",
  taskId: "task-1",
  toolUseId: "t1",
  tool: "run_python",
  title: "Run Python code",
  detail: "print(1)\n\nRuns natively with your account's privileges.",
  reason: "Running code natively is beyond this project's permissions (Code: restricted).",
  forTask: ["nativeCode"],
};

describe("approval card", () => {
  beforeEach(() => {
    core.invoke.mockReset().mockResolvedValue(undefined);
    ipc.agentsApprovalResolve.mockReset().mockResolvedValue(undefined);
  });
  afterEach(() => cleanup());

  it("offers Approve for this task for repeatable kinds and sends the task scope", async () => {
    render(<ApprovalCard approval={approval} />);
    expect(screen.getByText(/beyond this project's permissions/)).toBeTruthy();
    expect(screen.getByTestId("agents-approval-scope").textContent).toContain("later native code");
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Approve for this task" }));
    });
    expect(core.invoke).toHaveBeenCalledWith("agents_approval_resolve", {
      task: "task-1",
      approval: "approval-1",
      approved: true,
      scope: "task",
    });
    expect(ipc.agentsApprovalResolve).not.toHaveBeenCalled();
  });

  it("keeps a one-off decision one-off: no Approve for this task", async () => {
    render(<ApprovalCard approval={{ ...approval, title: "Move 1 item to the Recycle Bin", forTask: [] }} />);
    expect(screen.queryByRole("button", { name: "Approve for this task" })).toBeNull();
    expect(screen.queryByTestId("agents-approval-scope")).toBeNull();
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    });
    expect(ipc.agentsApprovalResolve).toHaveBeenCalledWith("task-1", "approval-1", true);
  });
});

describe("composer Ask / Auto", () => {
  const noRecord: ProjectPermissionsState = {
    root: "C:/paper",
    project: "prj-1",
    permissions: null,
    defaults: { files: "edit", code: "restricted", setup: false },
    restrictedAvailable: true,
    trusted: true,
  };
  const granted: ProjectPermissionsState = {
    ...noRecord,
    permissions: { version: 5, grantedAtMs: 1, updatedAtMs: 1, files: "edit", code: "restricted", setup: false },
  };
  let state = noRecord;
  beforeEach(() => {
    state = noRecord;
    core.invoke.mockReset().mockImplementation((command: string) => {
      if (command === "agents_permissions_get") return Promise.resolve(state);
      if (command === "agents_permissions_set") {
        state = granted;
        return Promise.resolve({ root: "C:/paper", project: "prj-1", permissions: granted.permissions, narrowed: [], stopped: [], problems: [] });
      }
      return Promise.resolve(undefined);
    });
    ipc.agentsTaskCreate.mockReset().mockResolvedValue({ id: "task-9" });
    useAgents.setState({
      ...initialAgentsState,
      status: {
        settings: { provider: "scripted", model: "m", effort: "low", consent: "scopeContent" },
        configured: false,
        credentialStore: "test",
        credentialError: null,
        scriptedAvailable: true,
      },
      workspace: { root: "C:/paper", name: "paper" } as never,
    });
  });
  afterEach(() => cleanup());

  it("needs the project's permissions before an Auto task can start, then sends Auto", async () => {
    render(<TaskComposer onCreated={() => {}} />);
    fireEvent.change(screen.getByTestId("agents-prompt"), { target: { value: "Run the analysis" } });
    // Ask: the task's own settings are shown.
    expect(screen.getByTestId("agents-permission")).toBeTruthy();
    fireEvent.click(screen.getByRole("radio", { name: "Auto" }));
    await waitFor(() => expect(screen.getByTestId("agents-auto-profile").getAttribute("data-granted")).toBe("false"));
    expect(screen.queryByTestId("agents-permission")).toBeNull();
    expect(screen.getByTestId("agents-permissions-suggested").textContent).toContain("Code: restricted container");
    expect(screen.getByTestId("agents-permissions-environment").textContent).toContain("never falls back");
    expect((screen.getByTestId("agents-start") as HTMLButtonElement).disabled).toBe(true);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Use these permissions" }));
    });
    expect(core.invoke).toHaveBeenCalledWith("agents_permissions_set", {
      root: "C:/paper",
      choice: { files: "edit", code: "restricted", setup: false },
    });
    await waitFor(() => expect(screen.getByTestId("agents-auto-profile").getAttribute("data-granted")).toBe("true"));
    expect((screen.getByTestId("agents-start") as HTMLButtonElement).disabled).toBe(false);
    await act(async () => {
      fireEvent.click(screen.getByTestId("agents-start"));
    });
    expect(ipc.agentsTaskCreate).toHaveBeenCalledWith(
      expect.objectContaining({ approvalMode: "auto", permission: "autoApply", execution: "approveEach" }),
    );
  });

  // XR-F-03: Read only with "Restricted sandbox only" looked like it allowed
  // execution; the service refuses every launch of a read-only task.
  it("shows that Read only allows no commands and sends no execution choice with it", async () => {
    const choose = async (label: RegExp, option: string) => {
      const field = screen.getByRole("button", { name: label });
      await act(async () => {
        fireEvent.click(field);
      });
      await act(async () => {
        fireEvent.click(await screen.findByRole("option", { name: option }));
      });
    };
    render(<TaskComposer onCreated={() => {}} />);
    fireEvent.change(screen.getByTestId("agents-prompt"), { target: { value: "Compute 2 + 2" } });
    await choose(/Commands and code/, "Restricted sandbox only (unattended)");
    await choose(/Document changes/, "Read only");
    const execution = screen.getByTestId("agents-execution");
    const button = within(execution).getByRole("button");
    expect(button.textContent).toContain("No commands");
    expect((button as HTMLButtonElement).disabled).toBe(true);
    expect(execution.textContent).toContain("Read only also allows no commands or code.");
    await act(async () => {
      fireEvent.click(screen.getByTestId("agents-start"));
    });
    expect(ipc.agentsTaskCreate).toHaveBeenCalledWith(
      expect.objectContaining({ approvalMode: "ask", permission: "readOnly", execution: "disabled" }),
    );
    // Allowing changes again restores the person's execution choice.
    await choose(/Document changes/, "Ask before each change");
    expect(within(screen.getByTestId("agents-execution")).getByRole("button").textContent).toContain(
      "Restricted sandbox only (unattended)",
    );
  });
});

describe("allowances and permission events", () => {
  afterEach(() => cleanup());

  it("lists the task's allowances and revokes one", async () => {
    core.invoke.mockReset().mockResolvedValue(undefined);
    ipc.agentsTaskDetail.mockReset().mockResolvedValue(undefined);
    useAgents.setState({
      ...initialAgentsState,
      details: {
        "task-1": {
          summary: {} as never,
          transcript: [],
          approvals: [],
          graph: { project: null, context: null, contexts: 0, targets: [], proposals: [], investigation: null },
          allowances: [{ kind: "nativeCode", grantedAtMs: 1, approved: "Run Python code" }],
        },
      },
    });
    render(<TaskAllowances id="task-1" />);
    expect(screen.getByTestId("agents-allowances").textContent).toContain("Native code");
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Revoke Native code" }));
    });
    expect(core.invoke).toHaveBeenCalledWith("agents_allowance_revoke", { task: "task-1", kind: "nativeCode" });
  });

  it("re-reads permissions on any change and reports stopped Auto tasks and unconfirmed cleanup", () => {
    const change = {
      root: "C:/paper",
      project: "prj-1",
      permissions: null,
      narrowed: ["revoked"],
      stopped: ["task-1"],
      problems: ["“Run the analysis” is still stopping."],
    };
    const effect = reduceEvent(initialAgentsState, { kind: "permissionsChanged", change });
    expect(effect.patch.permissionsVersion).toBe(1);
    expect(effect.notification?.title).toBe("Project permissions revoked");
    expect(effect.notification?.body).toContain("1 Auto task stopped; what they did stays reviewable.");
    expect(effect.notification?.body).toContain("still stopping");
    expect(effect.notification?.timeoutMs).toBeNull();
    expect(permissionsNotification({ ...change, stopped: [], problems: [] })).toBeUndefined();
  });
});
