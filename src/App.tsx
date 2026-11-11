import "./ui/tokens.css";
import "./ui/components/components.css";

import { Workbench } from "./app/Workbench";
import { installTheme } from "./ui/theme";

// Apply the persisted/OS theme before the first render.
installTheme();

export function App() {
  return <Workbench />;
}
