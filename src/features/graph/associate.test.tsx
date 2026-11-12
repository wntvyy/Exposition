import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

const lean = vi.hoisted(() => ({ associate: vi.fn() }));
vi.mock("../lean/evidence", () => ({ leanEvidenceCommands: lean }));

const { AssociateDeclarationAction, AssociateDeclarationDialog, CORRESPONDENCE_NOTE } = await import("./AssociateDeclaration");
const { entity } = await import("./fakeApi");

afterEach(() => {
  cleanup();
  lean.associate.mockReset();
});

const theorem = entity("thm", "theorem", "Main theorem");
const decl = entity("d1", "formalDeclaration", "LeanBasic.double_zero", { key: "LeanBasic.double_zero" });

describe("Associate a Lean declaration", () => {
  it("records a reviewed correspondence by declaration entity and says it is not a proof of equivalence", async () => {
    lean.associate.mockResolvedValue({
      association: { declaration: "d1", target: "thm", relation: "r1", status: "accepted", existing: false },
      evidence: null,
    });
    const onClose = vi.fn();
    render(<AssociateDeclarationDialog target={theorem} declarations={[decl]} onClose={onClose} />);
    expect(screen.getByTestId("graph-associate-note").textContent).toBe(CORRESPONDENCE_NOTE);
    expect(CORRESPONDENCE_NOTE).toMatch(/not a machine proof that they are equivalent/);
    fireEvent.change(screen.getByRole("textbox", { name: "Note (optional)" }), { target: { value: " Both state double 0 = 0 " } });
    fireEvent.click(screen.getByRole("button", { name: "Record reviewed association" }));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(lean.associate).toHaveBeenCalledWith({ declaration: { kind: "entity", entity: "d1" }, target: "thm", note: "Both state double 0 = 0" });
  });

  it("shows the backend's refusal and stays open", async () => {
    lean.associate.mockRejectedValue({ code: "lean.notDeclaration", message: "only a formal declaration can formalize a statement" });
    const onClose = vi.fn();
    render(<AssociateDeclarationDialog target={theorem} declarations={[decl]} onClose={onClose} />);
    fireEvent.click(screen.getByRole("button", { name: "Record reviewed association" }));
    expect((await screen.findByTestId("graph-associate-error")).textContent).toMatch(/only a formal declaration/);
    expect(onClose).not.toHaveBeenCalled();
  });

  it("with several declarations, nothing is preselected: the person chooses", () => {
    const other = entity("d0", "formalDeclaration", "LeanBasic.add_comm'", { key: "LeanBasic.add_comm'" });
    render(<AssociateDeclarationDialog target={theorem} declarations={[other, decl]} onClose={vi.fn()} />);
    const record = screen.getByRole("button", { name: "Record reviewed association" });
    expect(record.hasAttribute("disabled") || record.hasAttribute("data-disabled")).toBe(true);
    expect(lean.associate).not.toHaveBeenCalled();
  });

  it("is offered only for statements and similar targets, and explains when no declaration exists", () => {
    render(<AssociateDeclarationAction target={theorem} declarations={[]} canWrite />);
    const button = screen.getByTestId("graph-associate-button");
    expect(button.hasAttribute("disabled") || button.hasAttribute("data-disabled")).toBe(true);
    expect(screen.getByTestId("graph-associate-unavailable").textContent).toMatch(/No formal declarations/);
    cleanup();
    render(<AssociateDeclarationAction target={entity("p", "proof", "A proof")} declarations={[decl]} canWrite />);
    expect(screen.queryByTestId("graph-associate-button")).toBeNull();
  });
});
