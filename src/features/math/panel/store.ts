// The app's one equation panel store (one draft at a time, shared by both surfaces).
import { EquationPanelStore } from "./draft";
import { environment } from "./surfaces";

export const equationStore = new EquationPanelStore(environment);
