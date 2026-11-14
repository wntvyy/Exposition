// Wording and grouping of plans/outcomes, clipboard path classification,
// paste destinations, and the operation menus (bindings shown, recoverable
// vs permanent kept apart, unavailable actions disabled with a reason).
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import type { OpPlan } from "../../bindings/OpPlan";

vi.mock("./ipc", () => ({ opCommands: {}, opEvents: { projectOp: vi.fn(async () => () => {}) } }));

import { useContextMenu } from "../../ui/components";
import { pasteFolder } from "../workspace/fileOps";
import { classifyAbsolute } from "./clipboard";
import { decisionAction, decisionGroups, decisionTitle, initialChoices, summarizeResult } from "./model";
import { OpContextMenu } from "./OpMenu";
import type { SurfaceActions } from "./surfaces";

beforeAll(() => {
  Object.defineProperty(navigator, "platform", { value: "Win32", configurable: true });
});
afterEach(() => cleanup());

const path = (p: string) => ({ kind: "path" as const, path: p });

describe("decision wording", () => {
  const plan: OpPlan = {
    plan: "p",
    request: { kind: { kind: "delete", permanent: true }, targets: [path("figures/plot.png")], actor: { kind: "user" } },
    effects: [{ target: path("figures/plot.png"), description: "Delete permanently", destination: null, recovery: { kind: "none" } }],
    refusals: [],
    impacts: [
      { kind: "recoveryLoss", target: path("figures/plot.png"), consumers: [], message: "It cannot be restored." },
      { kind: "manuscriptDependency", target: path("figures/plot.png"), consumers: [], message: "paper.typ shows it." },
      { kind: "savedVersionCopied", target: path("figures/plot.png"), consumers: [], message: "notice" },
    ],
    needsDecision: true,
    choices: [{ key: "unsavedBuffers", label: "Unsaved text", options: [{ value: "preserve", label: "Keep in editor" }, { value: "discard", label: "Discard" }], default: "preserve" }],
    planHash: "h",
    expiresAtMs: 0,
  };

  it("names the resource and the action, never a generic question", () => {
    expect(decisionTitle(plan)).toBe("Permanently delete “plot.png”");
    expect(decisionAction(plan)).toBe("Delete Permanently");
  });

  it("groups decision impacts in a fixed order and leaves the saved-version notice out", () => {
    expect(decisionGroups(plan).map((g) => g.kind)).toEqual(["manuscriptDependency", "recoveryLoss"]);
  });

  it("choices start at their non-destructive default; presets only for offered values", () => {
    expect(initialChoices(plan.choices)).toEqual({ unsavedBuffers: "preserve" });
    expect(initialChoices(plan.choices, { unsavedBuffers: "discard", other: "x" })).toEqual({ unsavedBuffers: "discard" });
    expect(initialChoices(plan.choices, { unsavedBuffers: "nonsense" })).toEqual({ unsavedBuffers: "preserve" });
  });
});

describe("result wording", () => {
  it("app-managed deletion says where to restore it", () => {
    const s = summarizeResult(
      { kind: "delete", permanent: false },
      { plan: "p", outcomes: [{ target: { kind: "workflow", workflow: "wf-1" }, status: "succeeded", code: null, message: null, created: null, recovery: { kind: "appManaged" }, restore: "t" }], notices: [] },
      new Map([["workflow:wf-1", "Fit model"]]),
    );
    expect(s.title).toBe("Deleted “Fit model”");
    expect(s.lines).toEqual(["It can be restored from Settings › Storage & History."]);
    expect(s.restoreTokens).toHaveLength(1);
  });

  it("permanent deletion promises nothing", () => {
    const s = summarizeResult(
      { kind: "delete", permanent: true },
      { plan: "p", outcomes: [{ target: path("a.txt"), status: "succeeded", code: null, message: null, created: null, recovery: { kind: "none" }, restore: null }], notices: [] },
    );
    expect(s.title).toBe("Permanently deleted “a.txt”");
    expect(s.lines).toEqual(["It cannot be restored."]);
  });

  it("paste names the destination and created copies", () => {
    const s = summarizeResult(
      { kind: "paste", destination: { kind: "folder", folder: "figures" } },
      {
        plan: "p",
        outcomes: [
          { target: { kind: "external", path: "C:\\Users\\me\\a.png" }, status: "succeeded", code: null, message: null, created: path("figures/a.png"), recovery: { kind: "none" }, restore: null },
          { target: path("b.png"), status: "succeeded", code: null, message: null, created: path("figures/b (2).png"), recovery: { kind: "none" }, restore: null },
        ],
        notices: [],
      },
    );
    expect(s.title).toBe("Pasted 2 items into figures/");
    expect(s.lines).toEqual(["Created figures/a.png, figures/b (2).png."]);
  });
});

describe("clipboard and destinations", () => {
  it("absolute OS paths inside the project become project paths; others stay external sources", () => {
    expect(classifyAbsolute(["C:\\Work\\Proj\\figures\\a.png", "C:\\Other\\b.png", "c:\\work\\proj"], "C:\\Work\\Proj")).toEqual([
      path("figures/a.png"),
      { kind: "external", path: "C:\\Other\\b.png" },
      { kind: "external", path: "c:\\work\\proj" },
    ]);
  });

  it("paste goes into a selected folder, beside a selected file, or the project folder", () => {
    expect(pasteFolder([])).toBeNull();
    expect(pasteFolder([{ path: "chapters", kind: "directory", name: "chapters" }])).toBe("chapters");
    expect(pasteFolder([{ path: "chapters/one.typ", kind: "file", name: "one.typ" }])).toBe("chapters");
    expect(pasteFolder([{ path: "main.typ", kind: "file", name: "main.typ" }])).toBeNull();
    expect(pasteFolder([{ path: "a", kind: "file", name: "a" }, { path: "b", kind: "file", name: "b" }])).toBeUndefined();
  });
});

function MenuHarness({ actions }: { actions: SurfaceActions }) {
  const menu = useContextMenu();
  return (
    <div>
      <button onContextMenu={menu.openFromEvent}>item</button>
      {menu.position && <OpContextMenu controller={menu} label="File actions" actions={actions} />}
    </div>
  );
}

describe("operation menus", () => {
  it("show bindings, keep Delete and Delete Permanently apart, and disable with a reason", () => {
    const ran: string[] = [];
    const actions: SurfaceActions = {
      copy: { enabled: true, run: () => void ran.push("copy") },
      paste: { enabled: false, reason: "Select one folder", run: () => void ran.push("paste") },
      duplicate: { enabled: true, run: () => void ran.push("duplicate") },
      delete: { enabled: true, description: "Moves to the Recycle Bin", run: () => void ran.push("delete") },
      deletePermanently: { enabled: true, description: "Cannot be restored", run: () => void ran.push("deletePermanently") },
    };
    render(<MenuHarness actions={actions} />);
    fireEvent.contextMenu(screen.getByText("item"), { button: 2, clientX: 40, clientY: 40 });
    const menu = screen.getByRole("menu", { name: "File actions" });
    const items = within(menu).getAllByRole("menuitem");
    expect(items.map((i) => i.querySelector("[slot=label]")?.textContent)).toEqual(["Copy", "Paste", "Duplicate", "Delete", "Delete Permanently"]);
    const del = items[3]!;
    const permanent = items[4]!;
    expect(del.textContent).toContain("Moves to the Recycle Bin");
    expect(del.textContent).toContain("Delete");
    expect(permanent.textContent).toContain("Cannot be restored");
    expect(permanent.getAttribute("data-tone")).toBe("danger");
    expect(items[0]!.textContent).toContain("Ctrl");
    expect(permanent.textContent).toMatch(/Shift.*Delete/);
    expect(items[1]!.getAttribute("aria-disabled")).toBe("true");
    expect(items[1]!.textContent).toContain("Select one folder");
    fireEvent.click(items[2]!);
    expect(ran).toEqual(["duplicate"]);
    expect(screen.queryByRole("menu")).toBeNull();
  });
});
