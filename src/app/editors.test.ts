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

  it("focuses a group that shows editors", () => {
    editors.open(res("a"), { group: "primary" });
    editors.open(res("b"), { group: "secondary" });
    editors.focusGroup("primary");
    expect(useEditors.getState().activeGroup).toBe("primary");
    editors.focusGroup("secondary");
    expect(useEditors.getState().activeGroup).toBe("secondary");
  });
});

/** The invariants every change keeps (see `EditorsState`). */
function expectValidGroups(context = "") {
  const { groups, activeGroup } = useEditors.getState();
  const anyEditor = groups.primary.editors.length + groups.secondary.editors.length > 0;
  const where = `${context}: ${JSON.stringify({ activeGroup, primary: groups.primary, secondary: groups.secondary })}`;
  if (anyEditor) expect(groups[activeGroup].editors.length, `active group holds an editor ${where}`).toBeGreaterThan(0);
  else expect(activeGroup, `no editors: the left group is active ${where}`).toBe("primary");
  if (groups.secondary.editors.length > 0) {
    expect(groups.primary.editors.length, `the right group never shows alone ${where}`).toBeGreaterThan(0);
  }
  for (const group of [groups.primary, groups.secondary]) {
    if (group.editors.length === 0) expect(group.activeKey, where).toBeNull();
    else expect(group.editors.map((e) => e.resource.key), `activeKey names an editor ${where}`).toContain(group.activeKey);
  }
}

describe("editor groups: the active group always exists and holds an editor", () => {
  beforeEach(() => {
    useEditors.setState({
      groups: {
        primary: { id: "primary", editors: [], activeKey: null },
        secondary: { id: "secondary", editors: [], activeKey: null },
      },
      activeGroup: "primary",
      focusRequest: 0,
    });
  });

  /** "Split Editor Right": the active editor opens in the right group, which becomes active. */
  function split() {
    editors.open(res("a.typ"), { group: "primary" });
    editors.open(res("a.typ"), { group: "secondary" });
    expect(useEditors.getState().activeGroup).toBe("secondary");
  }

  it("closing the right group (command or its close button) activates the left group", async () => {
    split();
    expect(await editors.closeGroup("secondary")).toBe(true);
    expect(useEditors.getState().activeGroup).toBe("primary");
    expect(editors.active()?.resource.key).toBe("a.typ");
    expectValidGroups("after closeGroup");
  });

  it("closing the right group's last tab activates the left group", async () => {
    split();
    await editors.closeInGroup("a.typ", "secondary");
    expect(useEditors.getState().activeGroup).toBe("primary");
    expect(editors.active()?.resource.key).toBe("a.typ");
    expectValidGroups("after closing the last tab");
  });

  it("closing a resource everywhere that empties the active group activates the other group", async () => {
    editors.open(res("ga"), { group: "primary" });
    editors.open(res("gb"), { group: "secondary" });
    expect(await editors.close("gb")).toBe(true);
    expect(useEditors.getState().activeGroup).toBe("primary");
    expect(editors.active()?.resource.key).toBe("ga");
    expectValidGroups("after close(b)");
  });

  it("closing the left group's last editor moves the right group into its place", async () => {
    editors.open(res("ga"), { group: "primary" });
    editors.open(res("gb"), { group: "secondary" });
    editors.open(res("gc"), { group: "secondary" });
    editors.activate("gb", "secondary");
    editors.focusGroup("primary");
    await editors.closeInGroup("ga", "primary");
    const { groups, activeGroup } = useEditors.getState();
    expect(activeGroup).toBe("primary");
    expect(groups.primary.editors.map((e) => e.resource.key)).toEqual(["gb", "gc"]);
    expect(groups.primary.activeKey).toBe("gb");
    expect(groups.secondary.editors).toEqual([]);
    expectValidGroups("after closing the left group");
  });

  it("closing a resource shown in both groups closes it in both, even when that empties the left group", async () => {
    editors.open(res("ga"), { group: "primary" });
    editors.open(res("ga"), { group: "secondary" });
    editors.open(res("gb"), { group: "secondary" });
    expect(await editors.close("ga")).toBe(true);
    expect(editors.all().map((e) => e.resource.key)).toEqual(["gb"]);
    expect(useEditors.getState().groups.primary.editors.map((e) => e.resource.key)).toEqual(["gb"]);
    expectValidGroups("after close(a)");
  });

  it("closing every file editor (switching workspace) leaves a valid active group", async () => {
    editors.open(res("file:a"), { group: "primary" });
    editors.open(res("settings:"), { group: "secondary" });
    editors.open(res("file:b"), { group: "secondary" });
    for (const editor of editors.all()) {
      if (editor.resource.key.startsWith("file:")) await editors.close(editor.resource.key);
      expectValidGroups(`after closing ${editor.resource.key}`);
    }
    expect(editors.active()?.resource.key).toBe("settings:");
    await editors.close("settings:");
    expect(useEditors.getState().activeGroup).toBe("primary");
    expectValidGroups("after closing all");
  });

  it("does not activate an empty group while another group has editors", () => {
    editors.open(res("ga"), { group: "primary" });
    editors.focusGroup("secondary");
    expect(useEditors.getState().activeGroup).toBe("primary");
    expect(editors.active()?.resource.key).toBe("ga");
  });

  it("asks the workbench to focus the surviving group only when a group closes", async () => {
    const requests = () => useEditors.getState().focusRequest;
    split();
    editors.open(res("gb"), { group: "secondary" });
    const before = requests();
    await editors.closeInGroup("gb", "secondary"); // the group still shows a.typ
    editors.focusGroup("primary");
    expect(requests()).toBe(before);
    await editors.closeGroup("secondary");
    expect(requests()).toBe(before + 1);
    await editors.close("a.typ"); // nothing survives
    expect(requests()).toBe(before + 1);
  });

  it("keeps the invariants through any sequence of group operations", async () => {
    let seed = 7;
    const random = (n: number) => {
      seed = (seed * 1103515245 + 12345) % 2147483648;
      return seed % n;
    };
    const keys = ["ga", "gb", "gc", "gd"];
    const groupIds = ["primary", "secondary"] as const;
    for (let step = 0; step < 600; step++) {
      const key = keys[random(keys.length)]!;
      const group = groupIds[random(2)]!;
      const op = random(6);
      if (op === 0 || op === 1) editors.open(res(key), { group });
      else if (op === 2) await editors.closeInGroup(key, group);
      else if (op === 3) await editors.close(key);
      else if (op === 4) editors.focusGroup(group);
      else if (random(4) === 0) await editors.closeGroup(group);
      else editors.activate(key);
      expectValidGroups(`step ${step} (op ${op} ${key} ${group})`);
    }
  });
});
