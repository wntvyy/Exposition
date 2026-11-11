// Line-ending classification. Monaco models hold exactly one end-of-line
// sequence, so only LF, CRLF, or terminator-free text can be mirrored exactly
// (UTF-16 offsets identical to the document service). Mixed text is shown
// read-only until the user explicitly normalizes it.
export type EolMode = "lf" | "crlf" | "none" | "mixed";

export function classifyEol(text: string): EolMode {
  let lf = false;
  let crlf = false;
  for (let i = 0; i < text.length; i++) {
    const c = text.charCodeAt(i);
    if (c === 13) {
      if (text.charCodeAt(i + 1) === 10) {
        crlf = true;
        i++;
      } else {
        return "mixed"; // a lone CR cannot be represented in a Monaco model
      }
    } else if (c === 10) {
      lf = true;
    }
    if (lf && crlf) return "mixed";
  }
  return crlf ? "crlf" : lf ? "lf" : "none";
}

/** Does inserted text use only `eol` as its line terminator? */
export function textFitsEol(text: string, eol: "\n" | "\r\n"): boolean {
  if (eol === "\n") return !text.includes("\r");
  return !/[\r\n]/.test(text.replace(/\r\n/g, ""));
}
