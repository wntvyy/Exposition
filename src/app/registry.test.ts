import { describe, expect, it } from "vitest";

import { registry } from "./registry";

const Icon = () => null;

describe("workbench registry", () => {
  it("orders views by location and order, and unregisters", () => {
    const off1 = registry.registerView({ id: "t.b", location: "panel", title: "B", icon: Icon, order: 2, render: () => null });
    const off2 = registry.registerView({ id: "t.a", location: "panel", title: "A", icon: Icon, order: 1, render: () => null });
    expect(registry.viewsAt("panel").map((v) => v.id)).toEqual(["t.a", "t.b"]);
    off1();
    off2();
    expect(registry.viewsAt("panel")).toEqual([]);
  });

  it("picks the highest-priority matching editor", () => {
    const off1 = registry.registerEditor({ id: "t.text", matches: () => true, modes: ["source"], component: () => null });
    const off2 = registry.registerEditor({
      id: "t.typst",
      priority: 10,
      matches: (r) => r.path?.endsWith(".typ") ?? false,
      modes: ["source", "page", "split"],
      component: () => null,
    });
    expect(registry.editorFor({ key: "file:a.typ", path: "a.typ", title: "a.typ" })?.id).toBe("t.typst");
    expect(registry.editorFor({ key: "file:a.txt", path: "a.txt", title: "a.txt" })?.id).toBe("t.text");
    off1();
    off2();
  });

  it("rejects duplicate ids and skips disabled commands", async () => {
    let ran = 0;
    const off = registry.registerCommand({ id: "t.cmd", title: "Run", enabled: () => false, run: () => void ran++ });
    expect(() => registry.registerCommand({ id: "t.cmd", title: "Again", run: () => {} })).toThrow();
    await registry.runCommand("t.cmd");
    expect(ran).toBe(0);
    off();
  });
});
