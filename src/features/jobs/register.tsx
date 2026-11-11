// Jobs feature registration: Jobs panel, Toolchains view, running-jobs status item.
import { useEffect } from "react";
import { ListChecks, Wrench } from "lucide-react";

import { layout } from "../../app/layout";
import { registry } from "../../app/registry";
import { StatusPill } from "../../ui/components";
import { JobsPanel } from "./JobsPanel";
import { ToolchainsView } from "./ToolchainsView";
import { loadToolchains, refreshJobs, startJobsTracking, useJobs } from "./store";
import "./trust";

export const JOBS_VIEW_ID = "jobs.panel";
export const TOOLCHAINS_VIEW_ID = "jobs.toolchains";

function RunningJobsStatus() {
  const running = useJobs((s) => s.jobs.filter((j) => j.info.result === null && j.kind !== "probe").length);
  useEffect(() => startJobsTracking(), []);
  if (running === 0) return null;
  return (
    <span data-testid="jobs-status">
      <StatusPill size="sm" status="running" label={running === 1 ? "1 job running" : `${running} jobs running`} />
    </span>
  );
}

registry.registerView({
  id: JOBS_VIEW_ID,
  location: "panel",
  title: "Jobs",
  icon: ListChecks,
  order: 30,
  render: () => <JobsPanel />,
});

registry.registerView({
  id: TOOLCHAINS_VIEW_ID,
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
  id: "jobs.show",
  title: "Show Jobs",
  category: "Jobs",
  run: () => layout.showView("panel", JOBS_VIEW_ID),
});

registry.registerCommand({
  id: "jobs.refresh",
  title: "Refresh Job List",
  category: "Jobs",
  run: () => refreshJobs(),
});

registry.registerCommand({
  id: "toolchains.show",
  title: "Show Toolchains",
  category: "Jobs",
  run: () => layout.showView("panel", TOOLCHAINS_VIEW_ID),
});

registry.registerCommand({
  id: "toolchains.refresh",
  title: "Re-check Installed Toolchains",
  category: "Jobs",
  run: async () => {
    layout.showView("panel", TOOLCHAINS_VIEW_ID);
    await loadToolchains(true);
  },
});
