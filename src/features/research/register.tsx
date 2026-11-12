// Research feature registration: Research panel and research commands.
import { FlaskConical, Microscope } from "lucide-react";

import { editors } from "../../app/editors";
import { layout } from "../../app/layout";
import { notify } from "../../app/notifications";
import { registry } from "../../app/registry";
import { ResearchPage } from "./ResearchPage";
import { ResearchPanel } from "./ResearchPanel";
import { RESEARCH_PAGE_ID, enableResearch, revealInResearch, startSpaceTracking } from "./space";
import {
  interruptSession,
  loadEnvironments,
  restartSession,
  runFile,
  runSelection,
  startResearchTracking,
  stopSession,
  useResearch,
  userSession,
} from "./store";

export const RESEARCH_VIEW_ID = "research.panel";

const CODE_EXTENSIONS = ["py", "ipy", "r", "jl"];

// The Research page: full code/data exploration suite (shares sessions, runs,
// and document models with the bottom Research panel and the Editor page).
registry.registerPage({
  id: RESEARCH_PAGE_ID,
  title: "Research",
  icon: Microscope,
  order: 20,
  render: () => <ResearchPage />,
});

registry.registerView({
  id: RESEARCH_VIEW_ID,
  location: "panel",
  title: "Research",
  icon: FlaskConical,
  order: 20,
  render: () => <ResearchPanel />,
});

const show = () => {
  layout.showView("panel", RESEARCH_VIEW_ID);
  startResearchTracking();
};

const hasSession = () => Boolean(userSession());

registry.registerCommand({
  id: "research.show",
  title: "Show Research",
  category: "Research",
  run: show,
});

registry.registerCommand({
  id: "research.runSelection",
  title: "Run Selection in Research Session",
  category: "Research",
  run: async () => {
    show();
    await runSelection();
  },
});

registry.registerCommand({
  id: "research.runFile",
  title: "Run File in Research Session",
  category: "Research",
  run: async () => {
    show();
    const path = editors.active()?.resource.path;
    const ext = path?.split(".").pop()?.toLowerCase();
    if (!path || !ext || !CODE_EXTENSIONS.includes(ext)) {
      notify({
        tone: "info",
        title: "No script is active",
        body: "Open a Python, R, or Julia file in the editor, then run it in the research session.",
      });
      return;
    }
    await runFile(path);
  },
});

registry.registerCommand({
  id: "research.interrupt",
  title: "Interrupt Research Kernel",
  category: "Research",
  enabled: () => useResearch.getState().sessions.some((s) => s.state.kind === "busy"),
  run: () => interruptSession(),
});

registry.registerCommand({
  id: "research.restart",
  title: "Restart Research Kernel",
  category: "Research",
  enabled: hasSession,
  run: () => restartSession(),
});

registry.registerCommand({
  id: "research.stop",
  title: "Stop Research Session",
  category: "Research",
  enabled: hasSession,
  run: () => stopSession(),
});

registry.registerCommand({
  id: "research.refreshEnvironments",
  title: "Re-check Python, R, and Julia Environments",
  category: "Research",
  run: async () => {
    show();
    await loadEnvironments(true);
  },
});

registry.registerCommand({
  id: "research.showPage",
  title: "Show Research Page",
  category: "Research",
  run: () => {
    startResearchTracking();
    startSpaceTracking();
    revealInResearch(null);
  },
});

registry.registerCommand({
  id: "research.enable",
  title: "Enable Research for This Project",
  category: "Research",
  enabled: () => Boolean(useResearch.getState().workspace),
  run: async () => {
    startSpaceTracking();
    revealInResearch(null);
    await enableResearch();
  },
});
