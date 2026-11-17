// The Equation panel wired to the app's one draft store and the editors.
import { useActiveEditor } from "../../../app/editors";
import { EquationView } from "./EquationView";
import { equationStore } from "./store";
import { editInSource } from "./surfaces";

export function EquationPanel() {
  const active = useActiveEditor();
  return (
    <EquationView
      store={equationStore}
      activePath={active?.resource.path ?? null}
      onEditInSource={(path, range) => void editInSource(path, range)}
    />
  );
}
