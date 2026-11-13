// Workflow contributions: the editor for `workflow:<id>` resources (so
// Research, Agents, and Graph can open drafts), the run monitor for
// `workflow-run:<id>`, the workflows list (`workflows:`), and palette
// commands to create drafts from the workflow presets. Nothing here starts a
// run: runs start only from a reviewed plan in the editor.
import { editors } from "../../app/editors";
import { registry } from "../../app/registry";
import { useWorkspace } from "../workspace/store";
import { newDraftFromPreset, WORKFLOWS_KEY } from "./actions";
import { WORKFLOW_KEY_PREFIX } from "./model";
import { notify } from "../../app/notifications";
import type { WorkflowRunState } from "../../bindings/WorkflowRunState";
import { workflowEvents } from "./ipc";
import { RUN_STATE_VIEW, WORKFLOW_RUN_KEY_PREFIX } from "./runModel";
import { openRun, WorkflowRunView } from "./RunView";
import { WorkflowEditor } from "./WorkflowEditor";
import { WorkflowsView } from "./WorkflowsView";
import "./workflows.css";

const projectOpen = () => useWorkspace.getState().info !== null;

registry.registerEditor({
  id: "workflows.editor",
  priority: 100,
  matches: (resource) => resource.key.startsWith(WORKFLOW_KEY_PREFIX),
  modes: ["source"],
  component: WorkflowEditor,
});

registry.registerEditor({
  id: "workflows.list",
  priority: 100,
  matches: (resource) => resource.key === WORKFLOWS_KEY,
  modes: ["source"],
  component: WorkflowsView,
});

registry.registerEditor({
  id: "workflows.run",
  priority: 100,
  matches: (resource) => resource.key.startsWith(WORKFLOW_RUN_KEY_PREFIX),
  modes: ["source"],
  component: WorkflowRunView,
});

registry.registerCommand({
  id: "workflows.show",
  title: "Show Workflows",
  category: "Workflow",
  enabled: projectOpen,
  run: () => editors.open({ key: WORKFLOWS_KEY, title: "Workflows" }),
});

registry.registerCommand({
  id: "workflows.newNumericalExperiment",
  title: "New Numerical Experiment Draft",
  category: "Workflow",
  enabled: projectOpen,
  run: async () => {
    await newDraftFromPreset("numericalExperiment");
  },
});

registry.registerCommand({
  id: "workflows.newFormalAudit",
  title: "New Formal Audit Draft",
  category: "Workflow",
  enabled: projectOpen,
  run: async () => {
    await newDraftFromPreset("formalAudit");
  },
});

registry.registerCommand({
  id: "workflows.import",
  title: "Import Workflow Draft…",
  category: "Workflow",
  enabled: projectOpen,
  run: () => editors.open({ key: WORKFLOWS_KEY, title: "Workflows" }),
});

// A run that ends while its tab is not in view still reports how it ended
// (success, failure, cancellation, interruption), once per run and state.
const announced = new Map<string, WorkflowRunState>();
const TONES = { succeeded: "success", failed: "danger", canceled: "info", interrupted: "warning" } as const;
void workflowEvents
  .run((event) => {
    if (event.state === "running" || announced.get(event.run) === event.state) return;
    announced.set(event.run, event.state);
    notify({
      id: `workflow-run-${event.run}`,
      tone: TONES[event.state],
      title: `Workflow ${RUN_STATE_VIEW[event.state].label.toLowerCase()}`,
      body: event.state === "interrupted" ? "The application stopped during this run; nothing was repeated automatically." : undefined,
      actions: [{ label: "Open run", run: () => openRun({ id: event.run, title: "Workflow" }) }],
    });
  })
  .catch(() => undefined);
