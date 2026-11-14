// Copy on a task card says what was copied (or that it could not be).
import { describe, expect, it, vi } from "vitest";

const notifications = vi.hoisted(() => ({ notify: vi.fn(), dismiss: vi.fn() }));
vi.mock("../../app/notifications", () => notifications);
const clipboard = vi.hoisted(() => ({ writeText: vi.fn(async () => true) }));
vi.mock("../operations/clipboard", () => clipboard);
vi.mock("../operations/client", () => ({ runUserOperation: vi.fn() }));

import type { TaskSummary } from "../../bindings/TaskSummary";
import { taskActions } from "./taskOps";

const task = { id: "task-1", title: "Summarize main.typ" } as unknown as TaskSummary;

describe("task card Copy", () => {
  it("copies the title and says so", async () => {
    await taskActions([task]).actions.copy!.run();
    expect(clipboard.writeText).toHaveBeenCalledWith("Summarize main.typ");
    expect(notifications.notify).toHaveBeenLastCalledWith(expect.objectContaining({ tone: "info", title: "Copied the title of “Summarize main.typ”" }));
  });

  it("says when the clipboard refused", async () => {
    clipboard.writeText.mockResolvedValueOnce(false);
    await taskActions([task]).actions.copy!.run();
    expect(notifications.notify).toHaveBeenLastCalledWith(expect.objectContaining({ tone: "warning", title: "Could not copy to the clipboard" }));
  });
});
