// Typesetting feature: build status and the problems panel.
import { createElement } from "react";
import { ListChecks } from "lucide-react";

import { registry } from "../../app/registry";
import { BuildStatus } from "./BuildStatus";
import { ProblemsPanel } from "./ProblemsPanel";
import { installTypesettingEvents } from "./store";

installTypesettingEvents();

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
