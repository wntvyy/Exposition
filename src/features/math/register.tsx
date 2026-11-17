// Visual equation editor: the Editor's "Equation" auxiliary view, its
// commands (palette, keybinding, source context menu, page toolbar), and the
// source-editor focus tracking that picks the surface an operation targets.
import { Sigma } from "lucide-react";

import { registry } from "../../app/registry";
import { registerEquationCommands } from "./panel/commands";
import { EquationPanel } from "./panel/EquationPanel";
import { EQUATION_VIEW_ID, installSourceFocusTracking } from "./panel/surfaces";

registry.registerView({
  id: EQUATION_VIEW_ID,
  location: "auxiliary",
  title: "Equation",
  icon: Sigma,
  order: 30,
  render: () => <EquationPanel />,
});

installSourceFocusTracking();
registerEquationCommands();
