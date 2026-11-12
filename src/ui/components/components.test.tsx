import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  Badge,
  confirmUnsaved,
  ConfirmHost,
  DataTable,
  EditorTabList,
  IconButton,
  NodeCard,
  StatusPill,
  Tabs,
  type EditorTabItem,
} from ".";

afterEach(cleanup);

describe("confirmUnsaved", () => {
  it("asks Save / Don't Save / Cancel in an alert dialog and resolves the choice", async () => {
    render(<ConfirmHost />);
    let result: Promise<string> | undefined;
    act(() => {
      result = confirmUnsaved("main.typ");
    });
    const dialog = screen.getByRole("alertdialog", { name: "Save changes to main.typ?" });
    expect(dialog).toBeTruthy();
    expect(screen.getAllByRole("button").map((b) => b.textContent)).toEqual(["Cancel", "Don't Save", "Save"]);
    fireEvent.click(screen.getByRole("button", { name: "Don't Save" }));
    await expect(result).resolves.toBe("discard");
    expect(screen.queryByRole("alertdialog")).toBeNull();
  });

  it("resolves cancel on Escape", async () => {
    render(<ConfirmHost />);
    let result: Promise<string> | undefined;
    act(() => {
      result = confirmUnsaved("a.md");
    });
    fireEvent.keyDown(screen.getByRole("alertdialog"), { key: "Escape", code: "Escape" });
    await expect(result).resolves.toBe("cancel");
  });
});

function TabsHarness({ onClose }: { onClose: (key: string) => void }) {
  const items: EditorTabItem[] = [
    { key: "a", title: "a.typ" },
    { key: "b", title: "b.py", dirty: true },
    { key: "c", title: "c.bib", preview: true },
  ];
  const [selected, setSelected] = useState("a");
  return (
    <Tabs selectedKey={selected} onSelectionChange={(k) => setSelected(String(k))}>
      <EditorTabList aria-label="Open editors" items={items} onClose={onClose} />
    </Tabs>
  );
}

describe("EditorTabList", () => {
  it("names dirty and preview tabs and closes on middle click, Delete, and the close affordance", () => {
    const onClose = vi.fn();
    render(<TabsHarness onClose={onClose} />);
    expect(screen.getByRole("tab", { name: "b.py, unsaved changes" })).toBeTruthy();
    expect(screen.getByRole("tab", { name: "c.bib, preview" })).toBeTruthy();

    fireEvent(screen.getByRole("tab", { name: "c.bib, preview" }), new MouseEvent("auxclick", { bubbles: true, button: 1 }));
    expect(onClose).toHaveBeenLastCalledWith("c");

    const first = screen.getByRole("tab", { name: "a.typ" });
    fireEvent.keyDown(first, { key: "Delete", code: "Delete" });
    expect(onClose).toHaveBeenLastCalledWith("a");

    const close = first.querySelector(".ui-editor-tab__close")!;
    fireEvent.click(close);
    expect(onClose).toHaveBeenLastCalledWith("a");
    expect(onClose).toHaveBeenCalledTimes(3);
  });
});

describe("IconButton and StatusPill", () => {
  it("requires and exposes an accessible name", () => {
    render(<IconButton icon={() => null} aria-label="Run cell" onPress={() => {}} />);
    expect(screen.getByRole("button", { name: "Run cell" })).toBeTruthy();
  });

  it("pairs every status with visible text", () => {
    render(
      <>
        <StatusPill status="failed" label="Build failed" detail="3 errors" />
        <StatusPill status="stale" />
      </>,
    );
    expect(screen.getByText("Build failed")).toBeTruthy();
    expect(screen.getByText("Stale")).toBeTruthy();
  });
});

describe("DataTable", () => {
  interface Item {
    id: string;
    name: string;
    size: number;
  }
  const rows: Item[] = [
    { id: "b", name: "beta", size: 2 },
    { id: "a", name: "alpha", size: 10 },
    { id: "c", name: "gamma", size: 1 },
  ];
  const columns = [
    { id: "name", title: "Name", isRowHeader: true, sortValue: (r: Item) => r.name, render: (r: Item) => r.name },
    { id: "size", title: "Size", sortValue: (r: Item) => r.size, render: (r: Item) => String(r.size) },
  ];

  it("renders a grid with a named row header, sorts, selects, and runs the row action", () => {
    const onSelect = vi.fn();
    const onAction = vi.fn();
    render(
      <DataTable
        aria-label="Items"
        columns={columns}
        rows={rows}
        rowKey={(r) => r.id}
        defaultSort={{ column: "name", direction: "ascending" }}
        onSelectionChange={onSelect}
        onRowAction={onAction}
      />,
    );
    const grid = screen.getByRole("grid", { name: "Items" });
    const names = () => [...grid.querySelectorAll('[role="rowheader"]')].map((c) => c.textContent);
    expect(names()).toEqual(["alpha", "beta", "gamma"]);
    fireEvent.click(screen.getByRole("columnheader", { name: "Size" }));
    expect(names()).toEqual(["gamma", "beta", "alpha"]);
    // Keyboard: focus the row, Space selects it, Enter runs its action.
    const beta = screen.getByRole("rowheader", { name: "beta" }).closest('[role="row"]') as HTMLElement;
    act(() => beta.focus());
    fireEvent.keyDown(beta, { key: " ", code: "Space" });
    fireEvent.keyUp(beta, { key: " ", code: "Space" });
    expect(onSelect).toHaveBeenLastCalledWith("b");
    fireEvent.keyDown(beta, { key: "Enter", code: "Enter" });
    fireEvent.keyUp(beta, { key: "Enter", code: "Enter" });
    expect(onAction).toHaveBeenLastCalledWith("b");
  });

  it("says what is missing when there are no rows", () => {
    render(<DataTable aria-label="Empty" columns={columns} rows={[]} rowKey={(r) => r.id} empty="No entities match" />);
    expect(screen.getByText("No entities match")).toBeTruthy();
  });
});

describe("Badge and NodeCard", () => {
  it("shows the type label as text next to the category color", () => {
    render(
      <NodeCard
        title="Main theorem"
        typeLabel="Theorem"
        category="math"
        badges={<Badge tone="stale">Stale</Badge>}
      />,
    );
    const card = screen.getByText("Main theorem").closest(".ui-node-card")!;
    expect(card.getAttribute("data-category")).toBe("math");
    expect(screen.getByText("Theorem")).toBeTruthy();
    expect(screen.getByText("Stale").closest(".ui-badge")?.getAttribute("data-tone")).toBe("stale");
  });
});
