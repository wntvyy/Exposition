import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { useEffect } from "react";
import { Network } from "lucide-react";
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";

import { resetLayoutForTests, layout } from "./layout";
import { EDITOR_PAGE, pages, resetPagesForTests, showPageCommandId, usePages } from "./pages";
import { registry } from "./registry";
import { focusNextRegion } from "./shell/regions";
import { Workbench } from "./Workbench";

beforeAll(() => {
  Object.defineProperty(window, "innerWidth", { configurable: true, value: 1440 });
  globalThis.ResizeObserver ??= class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
  globalThis.matchMedia ??= ((query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addEventListener() {},
    removeEventListener() {},
    addListener() {},
    removeListener() {},
    dispatchEvent: () => false,
  })) as unknown as typeof matchMedia;
});

const offs: (() => void)[] = [];

beforeEach(() => {
  localStorage.clear();
  resetLayoutForTests();
  resetPagesForTests();
  layout.setViewportWidth(1440);
});

afterEach(() => {
  cleanup();
  while (offs.length) offs.pop()!();
});

describe("pages", () => {
  it("refuses the reserved editor id and unknown pages", () => {
    expect(() => registry.registerPage({ id: EDITOR_PAGE, title: "x", icon: Network, render: () => null })).toThrow();
    expect(pages.show("nope")).toBe(false);
    expect(usePages.getState().active).toBe(EDITOR_PAGE);
  });

  it("mounts a page on first visit and keeps it mounted (hidden, inert) after leaving", () => {
    let mounts = 0;
    function Probe() {
      useEffect(() => {
        mounts += 1;
      }, []);
      return <p>graph page body</p>;
    }
    offs.push(registry.registerPage({ id: "t.graph", title: "Graph", icon: Network, order: 40, render: () => <Probe /> }));
    render(<Workbench />);
    expect(screen.queryByText("graph page body")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Graph" }));
    expect(usePages.getState().active).toBe("t.graph");
    const body = screen.getByText("graph page body");
    expect(body.closest("[data-page]")?.hasAttribute("hidden")).toBe(false);
    const mountedOnce = mounts;

    fireEvent.click(screen.getByRole("button", { name: "Editor" }));
    expect(usePages.getState().active).toBe(EDITOR_PAGE);
    const page = document.querySelector('[data-page="t.graph"]')!;
    expect(page.hasAttribute("hidden")).toBe(true);
    expect(page.hasAttribute("inert")).toBe(true);

    act(() => void pages.show("t.graph"));
    expect(mounts).toBe(mountedOnce);
  });

  it("marks the shown page with aria-current and names every page button", () => {
    offs.push(registry.registerPage({ id: "graph", title: "Graph", icon: Network, order: 40, render: () => <p>g</p> }));
    render(<Workbench />);
    const pagesGroup = screen.getByRole("toolbar", { name: "Pages" });
    const editorButton = within(pagesGroup).getByRole("button", { name: "Editor" });
    const graphButton = within(pagesGroup).getByRole("button", { name: "Graph" });
    expect(editorButton.getAttribute("aria-current")).toBe("page");
    expect(graphButton.getAttribute("aria-current")).toBeNull();
    fireEvent.click(graphButton);
    expect(graphButton.getAttribute("aria-current")).toBe("page");
    expect(editorButton.getAttribute("aria-current")).toBeNull();
    // Choosing the shown page again keeps it (navigation, not a toggle).
    fireEvent.click(graphButton);
    expect(usePages.getState().active).toBe("graph");
  });

  it("registers a View: Show <page> command per page, with the product page keybindings", async () => {
    offs.push(registry.registerPage({ id: "graph", title: "Graph", icon: Network, order: 40, render: () => <p>g</p> }));
    render(<Workbench />);
    expect(registry.command(showPageCommandId("editor"))).toMatchObject({ title: "Show Editor", category: "View", keybinding: "Mod+1" });
    expect(registry.command(showPageCommandId("graph"))).toMatchObject({ title: "Show Graph", keybinding: "Mod+4" });

    // A page registered later gets its command too, and loses it when it unregisters.
    const off = registry.registerPage({ id: "t.later", title: "Later", icon: Network, order: 90, render: () => null });
    expect(registry.command(showPageCommandId("t.later"))?.title).toBe("Show Later");
    off();
    expect(registry.command(showPageCommandId("t.later"))).toBeUndefined();

    await act(async () => registry.runCommand(showPageCommandId("graph")));
    expect(usePages.getState().active).toBe("graph");
  });

  it("switches pages with Mod+<digit> even from a text field, and focuses the page", async () => {
    offs.push(
      registry.registerPage({
        id: "graph",
        title: "Graph",
        icon: Network,
        order: 40,
        render: () => (
          <div>
            <div data-region="graph-toolbar">
              <button type="button">Fit view</button>
            </div>
          </div>
        ),
      }),
    );
    render(<Workbench />);
    const input = document.createElement("input");
    document.body.appendChild(input);
    input.focus();
    fireEvent.keyDown(input, { key: "4", code: "Digit4", ctrlKey: true });
    expect(usePages.getState().active).toBe("graph");
    await waitFor(() => expect(document.activeElement?.textContent).toBe("Fit view"));
    fireEvent.keyDown(document.activeElement!, { key: "1", code: "Digit1", ctrlKey: true });
    expect(usePages.getState().active).toBe(EDITOR_PAGE);
    input.remove();
  });

  it("cycles F6 through the inner regions of a page (and the page itself when it has none)", () => {
    offs.push(
      registry.registerPage({
        id: "t.parts",
        title: "Parts",
        icon: Network,
        order: 40,
        render: () => (
          <>
            <div data-region="t-main">
              <button type="button">main action</button>
            </div>
            <div data-region="t-inspector">
              <button type="button">inspector action</button>
            </div>
          </>
        ),
      }),
      registry.registerPage({ id: "t.plain", title: "Plain", icon: Network, order: 50, render: () => <button type="button">plain</button> }),
    );
    render(<Workbench />);
    act(() => void pages.show("t.parts"));
    const visited: (string | undefined)[] = [];
    for (let i = 0; i < 4; i++) {
      act(() => focusNextRegion(false));
      visited.push((document.activeElement?.closest("[data-region]") as HTMLElement | null)?.dataset.region);
    }
    expect(visited).toEqual(["activitybar", "t-main", "t-inspector", "statusbar"]);

    act(() => void pages.show("t.plain"));
    act(() => focusNextRegion(false));
    expect((document.activeElement?.closest("[data-region]") as HTMLElement | null)?.dataset.region).toBe("activitybar");
    act(() => focusNextRegion(false));
    expect(document.activeElement?.textContent).toBe("plain");
  });

  it("region toggles on another page show the Editor page with that region", async () => {
    offs.push(registry.registerPage({ id: "graph", title: "Graph", icon: Network, order: 40, render: () => <p>g</p> }));
    render(<Workbench />);
    act(() => layout.setVisible("sidebar", false));
    act(() => void pages.show("graph"));
    await act(async () => registry.runCommand("workbench.action.toggleSidebar"));
    expect(usePages.getState().active).toBe(EDITOR_PAGE);
    expect(layout.isVisible("sidebar")).toBe(true);
  });
});
