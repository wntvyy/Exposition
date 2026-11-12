import { ArrowDown, ArrowUp } from "lucide-react";
import { useMemo, useState, type ReactNode } from "react";
import {
  Cell,
  Column,
  Row,
  Table,
  TableBody,
  TableHeader,
  type Key,
  type Selection,
  type SortDescriptor,
} from "react-aria-components";

import { cx } from "./cx";

export interface DataTableColumn<T> {
  id: string;
  title: string;
  /** The column that names the row for assistive technology (exactly one). */
  isRowHeader?: boolean;
  /** Enables sorting by this column. */
  sortValue?: (row: T) => string | number;
  /** CSS width, e.g. "30%" or "120px". */
  width?: string;
  render: (row: T) => ReactNode;
}

export interface DataTableProps<T> {
  "aria-label": string;
  columns: DataTableColumn<T>[];
  rows: T[];
  rowKey: (row: T) => string;
  /** The selected row (single selection), or null. */
  selectedKey?: string | null;
  onSelectionChange?: (key: string | null) => void;
  /** Enter or double click on a row. */
  onRowAction?: (key: string) => void;
  /** Initial sort column and direction. */
  defaultSort?: { column: string; direction: "ascending" | "descending" };
  /** Shown in the body when there are no rows. */
  empty?: ReactNode;
  /** Extra attributes for a row (e.g. `data-*` for tests). */
  rowProps?: (row: T) => Record<string, string | undefined>;
  className?: string;
}

function compare(a: string | number, b: string | number): number {
  if (typeof a === "number" && typeof b === "number") return a - b;
  return String(a).localeCompare(String(b), undefined, { numeric: true, sensitivity: "base" });
}

/**
 * Keyboard-accessible data table (grid semantics): arrow keys move between
 * rows and cells, Space/click selects a row, Enter or double click runs the
 * row action, column headers sort. Use for list views of structured records
 * and as the keyboard alternative to canvases.
 */
export function DataTable<T>({
  columns,
  rows,
  rowKey,
  selectedKey,
  onSelectionChange,
  onRowAction,
  defaultSort,
  empty,
  rowProps,
  className,
  ...aria
}: DataTableProps<T>) {
  const [sort, setSort] = useState<SortDescriptor | undefined>(
    defaultSort ? { column: defaultSort.column, direction: defaultSort.direction } : undefined,
  );
  const sorted = useMemo(() => {
    const column = sort ? columns.find((c) => c.id === sort.column) : undefined;
    if (!column?.sortValue) return rows;
    const value = column.sortValue;
    const factor = sort?.direction === "descending" ? -1 : 1;
    return rows
      .map((row, index) => ({ row, index }))
      .sort((a, b) => factor * compare(value(a.row), value(b.row)) || a.index - b.index)
      .map(({ row }) => row);
  }, [rows, columns, sort]);
  const items = useMemo(() => sorted.map((row) => ({ id: rowKey(row), row })), [sorted, rowKey]);
  const selected: Selection = selectedKey ? new Set<Key>([selectedKey]) : new Set<Key>();

  return (
    <div className={cx("ui-data-table", className)}>
      <Table
        aria-label={aria["aria-label"]}
        className="ui-data-table__table"
        selectionMode={onSelectionChange ? "single" : "none"}
        selectionBehavior="replace"
        selectedKeys={selected}
        onSelectionChange={(keys) => {
          if (!onSelectionChange) return;
          const first = keys === "all" ? undefined : [...keys][0];
          onSelectionChange(first === undefined ? null : String(first));
        }}
        onRowAction={onRowAction ? (key) => onRowAction(String(key)) : undefined}
        sortDescriptor={sort}
        onSortChange={setSort}
      >
        <TableHeader className="ui-data-table__header" columns={columns}>
          {(column) => (
            <Column
              id={column.id}
              isRowHeader={column.isRowHeader}
              allowsSorting={Boolean(column.sortValue)}
              className="ui-data-table__column"
              style={column.width ? { width: column.width } : undefined}
            >
              {({ sortDirection, allowsSorting }) => (
                <span className="ui-data-table__column-label">
                  {column.title}
                  {allowsSorting &&
                    sortDirection &&
                    (sortDirection === "ascending" ? (
                      <ArrowUp size={12} aria-hidden />
                    ) : (
                      <ArrowDown size={12} aria-hidden />
                    ))}
                </span>
              )}
            </Column>
          )}
        </TableHeader>
        <TableBody
          className="ui-data-table__body"
          items={items}
          renderEmptyState={() => <div className="ui-data-table__empty">{empty ?? "No items"}</div>}
        >
          {(item) => (
            <Row id={item.id} columns={columns} className="ui-data-table__row" {...(rowProps?.(item.row) ?? {})}>
              {(column) => <Cell className="ui-data-table__cell">{column.render(item.row)}</Cell>}
            </Row>
          )}
        </TableBody>
      </Table>
    </div>
  );
}
