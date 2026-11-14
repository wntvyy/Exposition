import { describe, expect, it } from "vitest";

import { descendantsOf, parsePowerShellJson } from "./processes.mjs";

describe("parsePowerShellJson", () => {
  it("accepts raw control characters inside strings (PowerShell 5.1 leaves some unescaped)", () => {
    const raw = `[{"ProcessId":4,"Name":"a\u0001b","CommandLine":"x\ty\u001fz"},{"ProcessId":8,"Name":"n","CommandLine":null}]\r\n`;
    expect(() => JSON.parse(raw)).toThrow();
    expect(parsePowerShellJson(raw)).toEqual([
      { ProcessId: 4, Name: "a\u0001b", CommandLine: "x\ty\u001fz" },
      { ProcessId: 8, Name: "n", CommandLine: null },
    ]);
  });

  it("wraps a single object and handles empty output", () => {
    expect(parsePowerShellJson('{"ProcessId":1}')).toEqual([{ ProcessId: 1 }]);
    expect(parsePowerShellJson("  \r\n")).toEqual([]);
  });
});

describe("descendantsOf", () => {
  // pid -> { parent, name, created (ms since the epoch, 0 when unknown) }
  const table = new Map([
    [100, { parent: 1, name: "exposition.exe", created: 5_000 }],
    [200, { parent: 100, name: "python.exe", created: 6_000 }],
    [300, { parent: 200, name: "conhost.exe", created: 6_100 }],
    // Started by a linker that had PID 100 and exited before the app got PID 100.
    [400, { parent: 100, name: "VCTIP.EXE", created: 4_000 }],
    // Creation time unknown: trusted as before.
    [500, { parent: 100, name: "unknown.exe", created: 0 }],
  ]);

  it("follows parent links down the tree", () => {
    const names = descendantsOf(table, 100).map((p) => p.name);
    expect(names).toContain("python.exe");
    expect(names).toContain("conhost.exe");
    expect(names).toContain("unknown.exe");
  });

  it("ignores a process whose parent id was reused by a newer process", () => {
    expect(descendantsOf(table, 100).map((p) => p.name)).not.toContain("VCTIP.EXE");
  });
});
