import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useEffect } from "react";
import { Network } from "lucide-react";
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";

import { resetLayoutForTests, layout } from "./layout";
import { EDITOR_PAGE, pages, resetPagesForTests, usePages } from "./pages";
import { registry } from "./registry";
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
});
