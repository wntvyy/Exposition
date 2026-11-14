// The Jobs panel list and details: each row selects its job with one button
// (Cancel stays a separate control), rows show quiet inline status while the
// details carry the one headline pill, and stderr styling stays in the panel.
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { JobSummary } from "../../bindings/JobSummary";

const actions = vi.hoisted(() => ({ selectJob: vi.fn(), cancelJob: vi.fn(), startJobsTracking: vi.fn(() => () => {}) }));
vi.mock("./store", async (importOriginal) => ({ ...(await importOriginal<typeof import("./store")>()), ...actions }));

const { useJobs } = await import("./store");
const { JobsPanel } = await import("./JobsPanel");

function job(id: string, label: string, state: JobSummary["state"]): JobSummary {
  return {
    info: { id, label, program: "pdflatex", args: ["-interaction=nonstopmode", "main.tex"], policy: { kind: "trustedNative" }, startedAtMs: 1_000, result: null },
    kind: "batch",
    state,
    workingDirectory: "C:/paper",
    refusal: null,
    pid: 4242,
  };
}

beforeEach(() => {
  useJobs.setState({ jobs: [job("job-1", "Build paper", "running"), job("job-2", "Check proofs", "queued")], selected: "job-1", outputs: {} });
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("JobsPanel", () => {
  it("selects a job from its row button and cancels without selecting", () => {
    const { container } = render(<JobsPanel />);
    const row = container.querySelector('li.exp-jobs__row[data-job-id="job-2"]')!;
    fireEvent.click(row.querySelector(".exp-jobs__select")!);
    expect(actions.selectJob).toHaveBeenCalledWith("job-2");

    actions.selectJob.mockClear();
    const running = container.querySelector('li.exp-jobs__row[data-job-id="job-1"]')!;
    fireEvent.click(within(running as HTMLElement).getByRole("button", { name: "Cancel Build paper" }));
    expect(actions.cancelJob).toHaveBeenCalledWith("job-1");
    expect(actions.selectJob).not.toHaveBeenCalled();
    expect(within(running as HTMLElement).getByRole("button", { name: "Build paper" }).getAttribute("aria-current")).toBe("true");
  });

  it("shows quiet status in rows and the headline pill in the details", () => {
    const { container } = render(<JobsPanel />);
    const rowStatus = container.querySelector('li.exp-jobs__row[data-job-id="job-1"] [data-testid="job-status"]')!;
    expect(rowStatus.getAttribute("data-job-status")).toBe("running");
    expect(rowStatus.querySelector(".ui-status-text")).not.toBeNull();
    expect(rowStatus.querySelector(".ui-status-pill")).toBeNull();

    const detail = screen.getByTestId("job-detail");
    expect(detail.querySelectorAll(".ui-status-pill")).toHaveLength(1);
    expect(within(detail).getByText("Program").nextElementSibling?.textContent).toBe("pdflatex");
    expect(within(detail).getByText("Arguments").nextElementSibling?.textContent).toBe("-interaction=nonstopmode main.tex");
    expect(within(detail).getByText("Policy").nextElementSibling?.textContent).toBe("Trusted native");
    expect(within(detail).getByTestId("job-pid").textContent).toBe("4242");
  });
});
