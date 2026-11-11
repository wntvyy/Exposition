import { registry } from "../registry";
import { useRegistryVersion } from "./useRegistry";
import { ViewErrorBoundary } from "./ViewErrorBoundary";

/** Bottom status bar: registry status items, left- and right-aligned. */
export function StatusBar() {
  useRegistryVersion();
  const render = (alignment: "left" | "right") =>
    registry.statusItemsAt(alignment).map((item) => (
      <div key={item.id} className="wb-statusbar__item" data-status-item={item.id}>
        <ViewErrorBoundary name={item.id} compact>{item.render()}</ViewErrorBoundary>
      </div>
    ));
  return (
    <footer className="wb-statusbar" data-region="statusbar" aria-label="Status bar">
      <div className="wb-statusbar__side">{render("left")}</div>
      <div className="wb-statusbar__side wb-statusbar__side--end">{render("right")}</div>
    </footer>
  );
}
