// Parsing the process list that scenarios read through PowerShell.
//
// Windows PowerShell 5.1's `ConvertTo-Json` can emit raw control characters
// inside strings (for example a process whose command line contains one),
// which `JSON.parse` rejects. With `-Compress` there is no whitespace between
// tokens, so every raw control character is inside a string: escaping them
// yields valid JSON with the same content.

/** Parse `ConvertTo-Json -Compress` output; always returns an array. */
export function parsePowerShellJson(text) {
  const escaped = text
    .trim()
    .replace(/[\u0000-\u001f]/g, (c) => `\\u${c.charCodeAt(0).toString(16).padStart(4, "0")}`);
  if (!escaped) return [];
  const value = JSON.parse(escaped);
  return Array.isArray(value) ? value : [value];
}
