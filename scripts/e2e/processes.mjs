// Parsing the process list that scenarios read through PowerShell.
//
// Windows PowerShell 5.1's `ConvertTo-Json` can emit raw control characters
// inside strings (for example a process whose command line contains one),
// which `JSON.parse` rejects. With `-Compress` there is no whitespace between
// tokens, so every raw control character is inside a string: escaping them
// yields valid JSON with the same content.

/**
 * PowerShell command listing every process with its creation time in ms
 * since the epoch (`Created`, 0 when Windows does not report one).
 */
export const PROCESS_LIST_COMMAND =
  "Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name,CommandLine," +
  "@{n='Created';e={ if ($_.CreationDate) { [int64](($_.CreationDate.ToUniversalTime() - [datetime]'1970-01-01').TotalMilliseconds) } else { 0 } }} " +
  "| ConvertTo-Json -Compress";

/**
 * Every descendant of `rootPid` in `table` (pid -> { parent, created, ... }).
 * Windows reuses process ids, and a process keeps the id of a parent that has
 * exited: a process created before the one now holding its parent id is not
 * that process's child (e.g. a build tool's helper whose parent id now belongs
 * to the app). Unknown creation times (0) are trusted.
 */
export function descendantsOf(table, rootPid) {
  const out = [];
  const queue = [rootPid];
  while (queue.length) {
    const parent = queue.shift();
    const parentCreated = table.get(parent)?.created ?? 0;
    for (const [pid, info] of table) {
      if (info.parent !== parent || out.some((p) => p.pid === pid)) continue;
      if (parentCreated && info.created && info.created < parentCreated) continue;
      out.push({ pid, ...info });
      queue.push(pid);
    }
  }
  return out;
}

/** Parse `ConvertTo-Json -Compress` output; always returns an array. */
export function parsePowerShellJson(text) {
  const escaped = text
    .trim()
    .replace(/[\u0000-\u001f]/g, (c) => `\\u${c.charCodeAt(0).toString(16).padStart(4, "0")}`);
  if (!escaped) return [];
  const value = JSON.parse(escaped);
  return Array.isArray(value) ? value : [value];
}
