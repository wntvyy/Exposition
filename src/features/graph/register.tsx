// Graph feature registration: the Graph page (order 40), the Editor's
// "Dependencies" auxiliary view, and commands. Nothing here touches the graph
// until a graph view is shown: ordinary use never needs it.
import { Network, Share2 } from "lucide-react";

import { layout } from "../../app/layout";
import { EDITOR_PAGE, pages } from "../../app/pages";
import { registry } from "../../app/registry";
import { useWorkspace } from "../workspace/store";
import { DependencyInspector, DEPENDENCIES_VIEW_ID } from "./DependencyInspector";
import { GraphPage } from "./GraphPage";
import { GRAPH_PAGE_ID, showGraphSection } from "./navigation";
import { graph } from "./store";

registry.registerPage({
  id: GRAPH_PAGE_ID,
  title: "Graph",
  icon: Network,
  order: 40,
  render: () => <GraphPage />,
});

registry.registerView({
  id: DEPENDENCIES_VIEW_ID,
  location: "auxiliary",
  title: "Dependencies",
  icon: Share2,
  order: 30,
  render: () => <DependencyInspector />,
});

registry.registerCommand({
  id: "graph.showDependencies",
  title: "Show Dependencies at Cursor",
  category: "Graph",
  run: () => {
    pages.show(EDITOR_PAGE);
    layout.showView("auxiliary", DEPENDENCIES_VIEW_ID);
  },
});

registry.registerCommand({
  id: "graph.showProject",
  title: "Show Project Graph",
  category: "Graph",
  run: () => showGraphSection("project", { focus: true }),
});

registry.registerCommand({
  id: "graph.showMathematics",
  title: "Show Mathematics",
  category: "Graph",
  run: () => showGraphSection("mathematics", { focus: true }),
});

// A different folder means a different project graph: re-read if in use.
let root = useWorkspace.getState().info?.root ?? null;
useWorkspace.subscribe((state) => {
  const next = state.info?.root ?? null;
  if (next === root) return;
  root = next;
  graph.workspaceChanged();
});
