// The page bar's project menu (workspace/register.tsx): Open Folder…, Open
// Recent, New Project…, and Close Folder are available while a project is
// open; a Recent entry opens through openWorkspace (the project switch).
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const store = vi.hoisted(() => ({ openWorkspace: vi.fn(async () => true) }));
vi.mock("./FilesView", () => ({ FilesView: () => null }));
vi.mock("./store", async () => {
  const { create } = await import("zustand");
  const useWorkspace = create(() => ({
    info: { root: "C:\\work\\alpha", name: "alpha", recovery: [] } as null | { root: string; name: string; recovery: [] },
    recent: [
      { path: "C:\\work\\alpha", name: "alpha", openedAt: 3 },
      { path: "C:\\work\\beta", name: "beta", openedAt: 2 },
    ],
  }));
  return {
    FILES_VIEW_ID: "workspace.files",
    useWorkspace,
    openWorkspace: store.openWorkspace,
    openFolderDialog: vi.fn(),
    closeWorkspace: vi.fn(),
    startWorkspaceEvents: vi.fn(),
    samePath: (a: string, b: string) => a.toLowerCase() === b.toLowerCase(),
  };
});

const { registry } = await import("../../app/registry");
await import("./register");
const { useWorkspace } = await import("./store");

function ProjectMenu() {
  const item = registry.statusItemsAt("pagebar").find((i) => i.id === "workspace.project");
  return <>{item?.render()}</>;
}

const ran: string[] = [];
let disposers: (() => void)[] = [];

beforeEach(() => {
  ran.length = 0;
  store.openWorkspace.mockClear();
  disposers = [
    registry.registerCommand({ id: "projects.new", title: "New Project…", category: "File", run: () => void ran.push("projects.new") }),
  ];
});

afterEach(() => {
  for (const dispose of disposers) dispose();
  cleanup();
});

describe("project menu", () => {
  it("offers Open Folder…, Open Recent (other projects), New Project… and Close Folder while a project is open", async () => {
    render(<ProjectMenu />);
    const button = screen.getByTestId("pagebar-project");
    expect(button.textContent).toContain("alpha");
    fireEvent.click(button);
    const items = await screen.findAllByRole("menuitem");
    expect(items.map((i) => i.textContent)).toEqual(
      expect.arrayContaining([expect.stringContaining("Open Folder…"), "Open Recent", "New Project…", "Close Folder"]),
    );
    fireEvent.click(screen.getByRole("menuitem", { name: "New Project…" }));
    await waitFor(() => expect(ran).toEqual(["projects.new"]));
  });

  it("opens a Recent project through the project switch", async () => {
    render(<ProjectMenu />);
    fireEvent.click(screen.getByTestId("pagebar-project"));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Open Recent" }));
    const recent = await screen.findByRole("menu", { name: "Open Recent" });
    // The open project is not offered again.
    expect(recent.textContent).not.toContain("alpha");
    fireEvent.click(screen.getByRole("menuitem", { name: "beta" }));
    await waitFor(() => expect(store.openWorkspace).toHaveBeenCalledWith("C:\\work\\beta"));
    expect(store.openWorkspace).toHaveBeenCalledTimes(1);
  });

  it("without a folder: no Close Folder", async () => {
    useWorkspace.setState({ info: null });
    render(<ProjectMenu />);
    expect(screen.getByTestId("pagebar-project").textContent).toContain("No folder open");
    fireEvent.click(screen.getByTestId("pagebar-project"));
    await screen.findAllByRole("menuitem");
    expect(screen.queryByRole("menuitem", { name: "Close Folder" })).toBeNull();
  });
});
