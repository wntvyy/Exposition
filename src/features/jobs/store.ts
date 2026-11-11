// Jobs state for the workbench: the job list (refreshed on lifecycle events),
// the selected job, and its bounded output (pulled by sequence number).
import { create } from "zustand";

import type { JobId } from "../../bindings/JobId";
import type { JobSummary } from "../../bindings/JobSummary";
import type { ToolchainInfo } from "../../bindings/ToolchainInfo";
import { notify } from "../../app/notifications";
import { commands, errorMessage, events } from "../../lib/ipc";
import { EMPTY_OUTPUT, type OutputBuffer, appendOutput } from "./model";

interface JobsState {
  jobs: JobSummary[];
  selected: JobId | null;
  showProbes: boolean;
  outputs: Record<string, OutputBuffer>;
  error: string | null;
  toolchains: ToolchainInfo[] | null;
  toolchainsLoading: boolean;
  toolchainsError: string | null;
}

export const useJobs = create<JobsState>(() => ({
  jobs: [],
  selected: null,
  showProbes: false,
  outputs: {},
  error: null,
  toolchains: null,
  toolchainsLoading: false,
  toolchainsError: null,
}));

let started = false;
let refreshTimer: ReturnType<typeof setTimeout> | null = null;
const outputInFlight = new Set<string>();
const outputPending = new Set<string>();
/** Finished jobs already reported to the user (trust prompts). */
const reported = new Set<string>();
let trustPrompt: ((root: string) => Promise<boolean>) | null = null;

/** Lets the trust dialog module register itself without an import cycle. */
export function setTrustPrompt(prompt: (root: string) => Promise<boolean>): void {
  trustPrompt = prompt;
}

export async function refreshJobs(): Promise<void> {
  try {
    const jobs = await commands.jobList();
    useJobs.setState({ jobs, error: null });
    reportRefusals(jobs);
  } catch (e) {
    useJobs.setState({ error: errorMessage(e) });
  }
}

function scheduleRefresh() {
  if (refreshTimer) return;
  refreshTimer = setTimeout(() => {
    refreshTimer = null;
    void refreshJobs();
  }, 75);
}

function reportRefusals(jobs: JobSummary[]) {
  for (const job of jobs) {
    if (reported.has(job.info.id) || !job.refusal) continue;
    reported.add(job.info.id);
    const root = job.refusal.workspaceRoot;
    if (job.refusal.code === "untrustedWorkspace" && root) {
      notify({
        id: `trust-${root}`,
        tone: "warning",
        title: "Workspace not trusted",
        body: `“${job.info.label}” was not run. ${job.refusal.message}`,
        timeoutMs: null,
        actions: trustPrompt ? [{ label: "Trust workspace…", run: () => void trustPrompt?.(root) }] : [],
      });
    }
  }
}

/** Pull new output for a job (coalesces concurrent requests). */
export async function pullOutput(id: JobId): Promise<void> {
  if (outputInFlight.has(id)) {
    outputPending.add(id);
    return;
  }
  outputInFlight.add(id);
  try {
    for (;;) {
      const current = useJobs.getState().outputs[id] ?? EMPTY_OUTPUT;
      const page = await commands.jobOutput(id, current.lastSeq, 128 * 1024);
      useJobs.setState((s) => ({
        outputs: {
          ...s.outputs,
          [id]: appendOutput(s.outputs[id] ?? EMPTY_OUTPUT, page.chunks, {
            truncated: page.truncated,
            complete: page.complete && !page.hasMore,
          }),
        },
      }));
      if (!page.hasMore) break;
    }
  } catch (e) {
    useJobs.setState({ error: errorMessage(e) });
  } finally {
    outputInFlight.delete(id);
    if (outputPending.delete(id)) void pullOutput(id);
  }
}

export function selectJob(id: JobId | null): void {
  useJobs.setState({ selected: id });
  if (id) void pullOutput(id);
}

export async function cancelJob(id: JobId): Promise<void> {
  try {
    await commands.jobCancel(id);
  } catch (e) {
    notify({ tone: "danger", title: "Could not cancel job", body: errorMessage(e) });
  }
  scheduleRefresh();
}

export async function loadToolchains(refresh: boolean): Promise<void> {
  useJobs.setState({ toolchainsLoading: true, toolchainsError: null });
  try {
    const toolchains = refresh ? await commands.toolchainsRefresh() : await commands.toolchainsList();
    useJobs.setState({ toolchains, toolchainsLoading: false });
  } catch (e) {
    useJobs.setState({ toolchainsLoading: false, toolchainsError: errorMessage(e) });
  }
  scheduleRefresh();
}

/** Start listening to job events (idempotent). */
export function startJobsTracking(): void {
  if (started) return;
  started = true;
  void events.jobs((event) => {
    scheduleRefresh();
    if (event.kind === "output" || event.kind === "finished") {
      const id = event.kind === "output" ? event.id : event.result.id;
      if (useJobs.getState().selected === id) void pullOutput(id);
    }
  });
  void events.jobsResync(() => scheduleRefresh());
  void refreshJobs();
}
