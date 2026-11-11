import { describe, expect, it, vi } from "vitest";

import { parseHtmlTable } from "./table";

describe("parseHtmlTable", () => {
  it("reads a pandas-style table as plain text", () => {
    const grid = parseHtmlTable(`<div><style>td{color:red}</style><table class="dataframe">
      <thead><tr><th></th><th>n</th><th>sq</th></tr></thead>
      <tbody><tr><th>0</th><td>1</td><td>1.5</td></tr><tr><th>1</th><td>2</td><td>a &amp; b</td></tr></tbody>
    </table></div>`);
    expect(grid?.head.map((r) => r.map((c) => c.text))).toEqual([["", "n", "sq"]]);
    expect(grid?.body.map((r) => r.map((c) => c.text))).toEqual([
      ["0", "1", "1.5"],
      ["1", "2", "a & b"],
    ]);
    expect(grid?.body[0]?.[0]?.header).toBe(true);
  });

  it("never executes or loads active content", () => {
    const alert = vi.fn();
    (window as unknown as { __researchProbe: () => void }).__researchProbe = alert;
    const grid = parseHtmlTable(`<table><tr><td>
        <script>window.__researchProbe()</script>
        <img src="x" onerror="window.__researchProbe()">
        <svg onload="window.__researchProbe()"></svg>safe</td></tr></table>`);
    expect(grid?.body[0]?.[0]?.text).toBe("safe");
    expect(alert).not.toHaveBeenCalled();
    // Nothing from the parsed document reached the live page.
    expect(document.querySelector("img[src='x']")).toBeNull();
  });

  it("clamps spans, bounds rows, and handles missing tables", () => {
    const rows = Array.from({ length: 5 }, (_, i) => `<tr><td colspan="9999">${i}</td></tr>`).join("");
    const grid = parseHtmlTable(`<table>${rows}</table>`, 3);
    expect(grid?.body).toHaveLength(3);
    expect(grid?.truncated).toBe(true);
    expect(grid?.totalRows).toBe(5);
    expect(grid?.body[0]?.[0]?.colSpan).toBe(50);
    expect(parseHtmlTable("<p>no table</p>")).toBeNull();
  });
});
