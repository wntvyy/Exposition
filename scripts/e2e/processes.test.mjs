import { describe, expect, it } from "vitest";

import { parsePowerShellJson } from "./processes.mjs";

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
