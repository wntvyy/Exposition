import { Button, Spinner, StatusPill } from "../../ui/components";
import type { ViewStore } from "./viewStore";

/** Saved-layout state of a view: saving, saved, or not saved (with the reason and Retry). */
export function SaveIndicator({ store }: { store: ViewStore }) {
  const save = store.use((s) => s.save);
  const error = store.use((s) => s.saveError);
  return (
    <span className="xp-graph-save" data-testid="graph-save-state" data-state={save} aria-live="polite">
      {save === "saving" || save === "pending" ? (
        <>
          <Spinner size="sm" aria-hidden /> <span className="xp-muted">Saving layout…</span>
        </>
      ) : save === "saved" ? (
        <span className="xp-muted">Layout saved</span>
      ) : save === "error" ? (
        <>
          <StatusPill size="sm" status="failed" label="Layout not saved" detail={error ?? undefined} />
          <Button size="sm" variant="ghost" onPress={() => void store.flush()}>
            Retry
          </Button>
        </>
      ) : null}
    </span>
  );
}
