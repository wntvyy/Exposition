import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { FileText } from "lucide-react";
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";

import { editors, useEditors } from "./editors";
import { commandPalette } from "./shell/CommandPalette";
import { layout, resetLayoutForTests, useLayout } from "./layout";
import { dismiss, notify, useNotifications } from "./notifications";
import { registry, type EditorProps } from "./registry";
import { Workbench } from "./Workbench";

beforeAll(() => {
  Object.defineProperty(window, "innerWidth", { configurable: true, value: 1440 });
  // jsdom lacks layout observers used by react-resizable-panels and Monaco.
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
  layout.setViewportWidth(1440);
  useEditors.setState({
    groups: {
      primary: { id: "primary", editors: [], activeKey: null },
      secondary: { id: "secondary", editors: [], activeKey: null },
    },
    activeGroup: "primary",
  });
});

afterEach(() => {
  act(() => commandPalette.close());
  cleanup();
  while (offs.length) offs.pop()!();
  for (const n of useNotifications.getState().items) dismiss(n.id);
});

function ctrl(key: string, extra: Partial<KeyboardEventInit> = {}) {
  const code = /^[a-z]$/i.test(key) ? `Key${key.toUpperCase()}` : key;
  fireEvent.keyDown(document.activeElement ?? document.body, { key, code, ctrlKey: true, ...extra });
}

describe("Workbench", () => {
  it("shows honest fallbacks when no feature has registered views or editors", () => {
    render(<Workbench />);
    expect(screen.getByText("No folder open")).toBeTruthy();
    expect(screen.getByText("No editor open")).toBeTruthy();
    expect(screen.getByText("No output")).toBeTruthy();
    expect(screen.getAllByText("Not available yet").length).toBeGreaterThan(0);
  });

  it("renders registered views and the editor chosen for a resource, with its mode switcher", () => {
    offs.push(
      registry.registerView({
        id: "t.files",
        location: "sidebar",
        title: "Files",
        icon: FileText,
        render: () => <p>files view body</p>,
      }),
      registry.registerEditor({
        id: "t.doc",
        matches: (r) => r.key.startsWith("doc:"),
        modes: ["source", "page", "split"],
        component: ({ resource, mode }: EditorProps) => (
          <p>
            editing {resource.title} in {mode}
          </p>
        ),
      }),
    );
    render(<Workbench />);
    expect(screen.getByText("files view body")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Files" })).toBeTruthy();

    act(() => editors.open({ key: "doc:main.typ", title: "main.typ" }));
    expect(screen.getByText("editing main.typ in source")).toBeTruthy();
    const modes = screen.getByRole("radiogroup", { name: "Document mode" });
    fireEvent.click(within(modes).getByRole("radio", { name: "Page" }));
    expect(screen.getByText("editing main.typ in page")).toBeTruthy();

    act(() => editors.setDirty("doc:main.typ", true));
    expect(screen.getByRole("tab", { name: /main\.typ, unsaved changes/ })).toBeTruthy();
  });

  it("toggles regions with keyboard shortcuts", async () => {
    render(<Workbench />);
    expect(document.querySelector('[data-region="sidebar"]')).not.toBeNull();
    ctrl("b");
    await waitFor(() => expect(document.querySelector('[data-region="sidebar"]')).toBeNull());
    expect(useLayout.getState().sidebar.visible).toBe(false);
    ctrl("j");
    await waitFor(() => expect(document.querySelector('[data-region="panel"]')).toBeNull());
    ctrl("b", { altKey: true });
    await waitFor(() => expect(document.querySelector('[data-region="auxiliary"]')).toBeNull());
  });

  it("opens the command palette, filters, and runs a command", async () => {
    render(<Workbench />);
    ctrl("P", { shiftKey: true });
    const input = await screen.findByRole("textbox", { name: "Command" });
    fireEvent.change(input, { target: { value: "toggle panel" } });
    const option = await screen.findByRole("menuitem", { name: /View: Toggle Panel/ });
    // Keyboard activation (Enter on the first match) is exercised in the real app (e2e ui-shell).
    fireEvent.click(option);
    await waitFor(() => expect(useLayout.getState().panel.visible).toBe(false));
    await waitFor(() => expect(screen.queryByRole("textbox", { name: "Command" })).toBeNull());
  });

  it("renders notifications and announces them in a live region", () => {
    render(<Workbench />);
    act(() => {
      notify({ tone: "danger", title: "Build failed", body: "main.typ:3:1 unknown variable" });
    });
    expect(screen.getByRole("region", { name: "Notifications" })).toBeTruthy();
    expect(screen.getByTestId("live-assertive").textContent).toContain("Build failed");
    fireEvent.click(screen.getByRole("button", { name: "Dismiss notification: Build failed" }));
    expect(screen.queryByRole("region", { name: "Notifications" })).toBeNull();
  });
});
