import { beforeEach, describe, expect, it } from "vitest";

import { editors, useEditors } from "./editors";

const res = (key: string) => ({ key, path: key, title: key });

describe("editors", () => {
  beforeEach(() => {
    useEditors.setState({
      groups: {
        primary: { id: "primary", editors: [], activeKey: null },
        secondary: { id: "secondary", editors: [], activeKey: null },
      },
      activeGroup: "primary",
    });
  });

  it("opens, deduplicates, and activates resources", () => {
    editors.open(res("a.typ"));
    editors.open(res("b.md"));
    editors.open(res("a.typ"), { mode: "page" });
    const group = useEditors.getState().groups.primary;
    expect(group.editors.map((e) => e.resource.key)).toEqual(["a.typ", "b.md"]);
    expect(group.activeKey).toBe("a.typ");
    expect(editors.active()?.mode).toBe("page");
  });

  it("replaces a clean preview tab with the next preview", () => {
    editors.open(res("a"), { preview: true });
    editors.open(res("b"), { preview: true });
    expect(editors.all().map((e) => e.resource.key)).toEqual(["b"]);
    editors.setDirty("b", true);
    editors.open(res("c"), { preview: true });
    expect(editors.all().map((e) => e.resource.key)).toEqual(["b", "c"]);
  });

  it("honors close guards and activates a neighbor", async () => {
    editors.open(res("a"));
    editors.open(res("b"));
    editors.open(res("c"));
    editors.activate("b");
    let allow = false;
    editors.registerCloseGuard("b", async () => allow);
    expect(await editors.close("b")).toBe(false);
    allow = true;
    expect(await editors.close("b")).toBe(true);
    expect(useEditors.getState().groups.primary.activeKey).toBe("c");
  });

  it("closes a resource in one group without consulting its guard while another group shows it", async () => {
    editors.open(res("a"), { group: "primary" });
    editors.open(res("a"), { group: "secondary" });
    let consulted = false;
    editors.registerCloseGuard("a", async () => {
      consulted = true;
      return false;
    });
    expect(await editors.closeInGroup("a", "secondary")).toBe(true);
    expect(consulted).toBe(false);
    expect(useEditors.getState().groups.secondary.editors).toEqual([]);
    expect(useEditors.getState().groups.primary.editors.map((e) => e.resource.key)).toEqual(["a"]);
    // Last view of the resource: the guard decides.
    expect(await editors.closeInGroup("a", "primary")).toBe(false);
    expect(consulted).toBe(true);
  });

  it("focuses a group, including an empty one", () => {
    editors.focusGroup("secondary");
    expect(useEditors.getState().activeGroup).toBe("secondary");
  });
});
