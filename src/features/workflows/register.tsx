// Workflow draft contributions: the draft editor for `workflow:<id>`
// resources (so Research, Agents, and Graph can open drafts), the drafts list
// (`workflows:`), and palette commands to create drafts from the workflow
// presets. Nothing here runs a workflow.
import { editors } from "../../app/editors";
import { registry } from "../../app/registry";
import { useWorkspace } from "../workspace/store";
import { newDraftFromPreset, WORKFLOWS_KEY } from "./actions";
import { WORKFLOW_KEY_PREFIX } from "./model";
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

registry.registerCommand({
  id: "workflows.show",
  title: "Show Workflow Drafts",
  category: "Workflow",
  enabled: projectOpen,
  run: () => editors.open({ key: WORKFLOWS_KEY, title: "Workflow Drafts" }),
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
  run: () => editors.open({ key: WORKFLOWS_KEY, title: "Workflow Drafts" }),
});
