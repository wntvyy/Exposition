// Typesetting feature: build status, the problems panel, and build
// diagnostics as squiggles in the source editor.
import { createElement } from "react";
import { ListChecks } from "lucide-react";

import { registry } from "../../app/registry";
import { BuildStatus } from "./BuildStatus";
import { installBuildMarkers } from "./markers";
import { ProblemsPanel } from "./ProblemsPanel";
import { installTypesettingEvents } from "./store";

installTypesettingEvents();
installBuildMarkers();

registry.registerView({
  id: "typesetting.problems",
  location: "panel",
  title: "Problems",
  icon: ListChecks,
  order: 10,
  render: () => createElement(ProblemsPanel),
});

registry.registerStatusItem({
  id: "typesetting.build",
  alignment: "left",
  order: 20,
  render: () => createElement(BuildStatus),
});
