// Mathematics: associate an existing formal declaration (a Lean declaration
// admitted by the Lean service) with an informal statement. This records a
// reviewed correspondence (`declaration formalizes statement`, accepted) — the
// reviewer's assertion, not a machine proof that the two are equivalent — and
// never changes the verifier verdict or freshness, which come only from the
// Lean service's evidence. Only a formal declaration can be the source; the
// backend refuses anything else (`lean.notDeclaration`).
import { Braces } from "lucide-react";
import { useState } from "react";

import { notify } from "../../app/notifications";
import type { Entity } from "../../bindings/Entity";
import type { LeanAssociation } from "../../bindings/LeanAssociation";
import { Button, Dialog, Select, TextField } from "../../ui/components";
import { leanEvidenceCommands } from "../lean/evidence";
import { errorText } from "./api";
import { graph, useGraph } from "./store";

/** Kinds a formal declaration can formalize (display aid; the backend validates). */
export const FORMALIZABLE = new Set(["conjecture", "lemma", "theorem", "proposition", "corollary", "claim", "definition", "assumption", "mathObject"]);

export const CORRESPONDENCE_NOTE =
  "This records your reviewed correspondence between the declaration and this statement. It is not a machine proof that they are equivalent, and it does not change the verifier verdict, which comes only from Lean's audit evidence.";

export interface AssociateDeclarationDialogProps {
  target: Entity;
  /** Formal declaration entities in the project. */
  declarations: Entity[];
  onClose: () => void;
  onAssociated?: (result: LeanAssociation) => void;
}

export function AssociateDeclarationDialog({ target, declarations, onClose, onAssociated }: AssociateDeclarationDialogProps) {
  const [declaration, setDeclaration] = useState<string | null>(declarations[0]?.id ?? null);
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const tooLong = note.length > 500;

  const submit = async () => {
    if (!declaration || tooLong) return;
    setBusy(true);
    setError(null);
    try {
      const result = await leanEvidenceCommands.associate({
        declaration: { kind: "entity", entity: declaration },
        target: target.id,
        note: note.trim() ? note.trim() : null,
      });
      const name = declarations.find((d) => d.id === declaration)?.label ?? "The declaration";
      notify({
        tone: "success",
        title: result.association.existing ? "Association already recorded" : "Reviewed association recorded",
        body: `${name} formalizes ${target.label} (a reviewed correspondence, not a proof of equivalence).`,
      });
      // The graph event follows; read again now so the views reflect it at once.
      graph.noteRevision(useGraph.getState().revision + 1);
      onAssociated?.(result);
      onClose();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      title={`Associate a Lean declaration with ${target.label}`}
      isOpen
      onOpenChange={(open) => !open && onClose()}
      footer={
        <>
          <Button variant="secondary" onPress={onClose}>
            Cancel
          </Button>
          <Button variant="primary" isDisabled={!declaration || tooLong} isPending={busy} onPress={() => void submit()}>
            Record reviewed association
          </Button>
        </>
      }
    >
      <div className="xp-graph-form" data-testid="graph-associate-dialog">
        <Select<string>
          label="Formal declaration"
          placeholder="Choose a declaration"
          options={declarations.map((d) => ({ id: d.id, label: d.label, description: d.key && d.key !== d.label ? d.key : undefined }))}
          value={declaration}
          onChange={setDeclaration}
        />
        <TextField
          label="Note (optional)"
          description="Why the declaration states the same thing (at most 500 characters)."
          value={note}
          onChange={setNote}
          isInvalid={tooLong}
          errorMessage={tooLong ? "The note is longer than 500 characters." : undefined}
        />
        <p className="xp-muted" data-testid="graph-associate-note">
          {CORRESPONDENCE_NOTE}
        </p>
        {error && (
          <p className="xp-inline-error" role="alert" data-testid="graph-associate-error">
            {error}
          </p>
        )}
      </div>
    </Dialog>
  );
}

/** The inspector action: enabled only when the project has formal declarations. */
export function AssociateDeclarationAction({
  target,
  declarations,
  canWrite,
}: {
  target: Entity;
  declarations: Entity[];
  canWrite: boolean;
}) {
  const [open, setOpen] = useState(false);
  if (!FORMALIZABLE.has(target.kind)) return null;
  const none = declarations.length === 0;
  return (
    <div className="xp-graph-associate">
      <Button size="sm" icon={Braces} isDisabled={none || !canWrite} onPress={() => setOpen(true)} data-testid="graph-associate-button">
        Associate a Lean declaration…
      </Button>
      {none && (
        <p className="xp-muted" data-testid="graph-associate-unavailable">
          No formal declarations in this project yet. Check a Lean file and record its audited declarations to associate one.
        </p>
      )}
      {open && <AssociateDeclarationDialog target={target} declarations={declarations} onClose={() => setOpen(false)} />}
    </div>
  );
}
