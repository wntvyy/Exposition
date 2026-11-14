import type { ReactNode } from "react";

import { cx } from "./cx";

export interface SimpleTableColumn<T> {
  id: string;
  title: ReactNode;
  /** CSS width, e.g. "30%" or "120px". */
  width?: string;
  /** Right-align with tabular figures (sizes, counts). */
  numeric?: boolean;
  /** Visually hidden header text (e.g. a checkbox column); still read by screen readers. */
  hideTitle?: boolean;
  /** This column names the row: cells render as `<th scope="row">`. */
  isRowHeader?: boolean;
  render: (row: T) => ReactNode;
}

export interface SimpleTableProps<T> {
  /** Accessible name (a caption is not drawn). */
  "aria-label": string;
  columns: SimpleTableColumn<T>[];
  rows: T[];
  rowKey: (row: T) => string;
  /** Shown as one full-width row when there are no rows. */
  empty?: ReactNode;
  /** Extra attributes for a row (`data-*` for tests and P1 operations). */
  rowProps?: (row: T) => Record<string, string | boolean | undefined>;
  className?: string;
  "data-testid"?: string;
  /** Set while the rows are being (re)loaded. */
  "aria-busy"?: boolean;
}

/**
 * A static table (semantic `<table>`) with the same look as DataTable: one
 * header treatment, row height, dividers, and numeric alignment. Use it where
 * rows are read, not navigated as a grid: plan review inputs, Storage &
 * History, tool locations. Rows may contain their own controls (checkboxes,
 * buttons), which keep their normal Tab order.
 */
export function SimpleTable<T>({ columns, rows, rowKey, empty, rowProps, className, ...aria }: SimpleTableProps<T>) {
  return (
    <div className={cx("ui-data-table", "ui-simple-table", className)} data-testid={aria["data-testid"]}>
      <table className="ui-data-table__table" aria-label={aria["aria-label"]} aria-busy={aria["aria-busy"] || undefined}>
        <thead>
          <tr>
            {columns.map((column) => (
              <th
                key={column.id}
                scope="col"
                className="ui-data-table__column"
                data-numeric={column.numeric || undefined}
                style={column.width ? { width: column.width } : undefined}
              >
                {column.hideTitle ? <span className="ui-visually-hidden">{column.title}</span> : column.title}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.length === 0 ? (
            <tr>
              <td className="ui-data-table__empty" colSpan={columns.length}>
                {empty ?? "No items"}
              </td>
            </tr>
          ) : (
            rows.map((row) => (
              <tr key={rowKey(row)} className="ui-data-table__row" {...(rowProps?.(row) ?? {})}>
                {columns.map((column) =>
                  column.isRowHeader ? (
                    <th key={column.id} scope="row" className="ui-data-table__cell" data-numeric={column.numeric || undefined}>
                      {column.render(row)}
                    </th>
                  ) : (
                    <td key={column.id} className="ui-data-table__cell" data-numeric={column.numeric || undefined}>
                      {column.render(row)}
                    </td>
                  ),
                )}
              </tr>
            ))
          )}
        </tbody>
      </table>
    </div>
  );
}
