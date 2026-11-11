// Inert HTML table extraction for kernel output.
//
// Kernel HTML is untrusted. It is parsed with DOMParser into a separate,
// non-live document (no browsing context: scripts never run, resources are not
// fetched, event handlers never fire) and only the text of the first table's
// cells is read. Nothing from the parsed document is inserted into the page;
// the view renders its own elements from plain strings.

export interface GridCell {
  text: string;
  header: boolean;
  colSpan: number;
  rowSpan: number;
}

export interface Grid {
  head: GridCell[][];
  body: GridCell[][];
  /** Rows beyond the display limit were omitted. */
  truncated: boolean;
  totalRows: number;
}

const MAX_SPAN = 50;

function span(value: string | null): number {
  const n = Number.parseInt(value ?? "1", 10);
  return Number.isFinite(n) ? Math.min(Math.max(n, 1), MAX_SPAN) : 1;
}

function cellsOf(row: Element): GridCell[] {
  const cells: GridCell[] = [];
  for (const cell of Array.from(row.children)) {
    const tag = cell.tagName.toLowerCase();
    if (tag !== "td" && tag !== "th") continue;
    cells.push({
      text: (cell.textContent ?? "").replace(/\s+/g, " ").trim(),
      header: tag === "th",
      colSpan: span(cell.getAttribute("colspan")),
      rowSpan: span(cell.getAttribute("rowspan")),
    });
  }
  return cells;
}

/** First table of `html` as plain-text cells, or null when there is none. */
export function parseHtmlTable(html: string, maxRows = 200): Grid | null {
  const doc = new DOMParser().parseFromString(html, "text/html");
  const table = doc.querySelector("table");
  if (!table) return null;
  // Script/style text is not table content.
  for (const node of Array.from(doc.querySelectorAll("script, style, template, noscript"))) node.remove();
  const head: GridCell[][] = [];
  const body: GridCell[][] = [];
  const hasThead = table.querySelector("thead") !== null;
  const rows = Array.from(table.querySelectorAll("tr")).filter((row) => row.closest("table") === table);
  rows.forEach((row, index) => {
    const cells = cellsOf(row);
    if (cells.length === 0) return;
    const inHead = hasThead
      ? row.parentElement?.tagName.toLowerCase() === "thead"
      : index === 0 && cells.every((c) => c.header);
    (inHead ? head : body).push(cells);
  });
  const totalRows = body.length;
  return { head, body: body.slice(0, maxRows), truncated: totalRows > maxRows, totalRows };
}
