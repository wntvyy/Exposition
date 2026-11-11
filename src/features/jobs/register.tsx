// Jobs feature registration: Jobs panel, Toolchains view, running-jobs status item.
import { useEffect } from "react";
import { ListChecks, Wrench } from "lucide-react";

import { registry } from "../../app/registry";
import { JobsPanel } from "./JobsPanel";
import { ToolchainsView } from "./ToolchainsView";
import { loadToolchains, refreshJobs, startJobsTracking, useJobs } from "./store";
import "./trust";

function RunningJobsStatus() {
  const running = useJobs((s) => s.jobs.filter((j) => j.info.result === null && j.kind !== "probe").length);
  useEffect(() => startJobsTracking(), []);
  return (
    <span className="exp-status-item" data-testid="jobs-status" aria-live="polite">
      {running === 0 ? "No jobs running" : running === 1 ? "1 job running" : `${running} jobs running`}
    </span>
  );
}

registry.registerView({
  id: "jobs.panel",
  location: "panel",
  title: "Jobs",
  icon: ListChecks,
  order: 30,
  render: () => <JobsPanel />,
});

registry.registerView({
  id: "jobs.toolchains",
  location: "panel",
  title: "Toolchains",
  icon: Wrench,
  order: 90,
  render: () => <ToolchainsView />,
});

registry.registerStatusItem({
  id: "jobs.running",
  alignment: "left",
  order: 50,
  render: () => <RunningJobsStatus />,
});

registry.registerCommand({
  id: "jobs.refresh",
  title: "Refresh job list",
  category: "Jobs",
  run: () => refreshJobs(),
});

registry.registerCommand({
  id: "toolchains.refresh",
  title: "Re-check installed toolchains",
  category: "Jobs",
  run: () => loadToolchains(true),
});

export { requestWorkspaceTrust } from "./trust";
