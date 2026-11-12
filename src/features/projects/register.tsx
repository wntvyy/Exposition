// Project creation contributions: the New Project page and the setup-recovery
// view (editor tabs), the `File: New Project…` command, and the check for
// interrupted creations whenever a project opens.
import { registry } from "../../app/registry";
import { NEW_PROJECT_KEY, SETUP_KEY_PREFIX } from "./model";
import { NewProjectView } from "./NewProjectView";
import { ProjectSetupView } from "./ProjectSetupView";
import { openNewProject, startProjectEvents } from "./store";
import "./projects.css";

startProjectEvents();

registry.registerEditor({
  id: "projects.new",
  priority: 100,
  matches: (resource) => resource.key === NEW_PROJECT_KEY,
  modes: ["source"],
  component: NewProjectView,
});

registry.registerEditor({
  id: "projects.setup",
  priority: 100,
  matches: (resource) => resource.key.startsWith(SETUP_KEY_PREFIX),
  modes: ["source"],
  component: ProjectSetupView,
});

registry.registerCommand({
  id: "projects.new",
  title: "New Project…",
  category: "File",
  run: () => openNewProject("preset"),
});

registry.registerCommand({
  id: "projects.newFromObjective",
  title: "New Project from an Objective…",
  category: "File",
  run: () => openNewProject("objective"),
});
