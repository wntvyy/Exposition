import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { confirmUnsaved, ConfirmHost, EditorTabList, IconButton, StatusPill, Tabs, type EditorTabItem } from ".";

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
