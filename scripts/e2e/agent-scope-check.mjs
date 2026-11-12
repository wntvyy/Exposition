// Evidence that an out-of-scope read was refused by the APPLICATION (not merely
// declined by the model). Shared by live-agents (check D, real provider) and
// agents-attention (validated against a real refusal with the scripted provider).
//
// `detail` is an `agents_task_detail` result. Passing requires that the model
// actually called read_file on `path`, that every such call ran and failed with
// the application's scope refusal (the tool result's `error` text, or a scope
// error `code` if one is added), and that no tool result (nor anything else in
// the transcript) contains the file's content.

const parse = (text) => {
  try {
    return JSON.parse(text);
  } catch {
    return null;
  }
};

/** A tool call that ran and was refused by the application's scope check. */
export function isScopeRefusal(call) {
  const result = parse(call.result ?? "");
  const code = typeof result?.code === "string" ? result.code : "";
  const error = typeof result?.error === "string" ? result.error : "";
  return call.status === "failed" && call.isError === true && (/scope/i.test(code) || /outside this task's scope/.test(error));
}

/**
 * @param {object} detail agents_task_detail result
 * @param {{ path: string, markers: string[] }} file out-of-scope file and strings only its content contains
 *   (no tool result may contain any of them; the first may not appear anywhere)
 */
export function scopeRefusalEvidence(detail, { path, markers }) {
  const calls = detail.transcript.filter((t) => t.kind === "toolCall");
  const attempts = calls.filter((t) => t.name === "read_file" && (t.input?.path ?? "").includes(path));
  const leakingResults = calls.filter((t) => markers.some((m) => (t.result ?? "").includes(m)));
  // The first marker is unique to the file (e.g. a theorem name): it may not
  // appear anywhere, model prose included.
  const leakedAnywhere = detail.transcript.some((t) => JSON.stringify(t).includes(markers[0]));
  const refusedByApp = attempts.length > 0 && attempts.every(isScopeRefusal);
  const outcome =
    attempts.length === 0
      ? "NOT ATTEMPTED: the model did not call read_file on the out-of-scope file (no enforcement evidence)"
      : refusedByApp
        ? "attempted and refused by the application"
        : "attempted but NOT refused with the application's scope error";
  return {
    ok: refusedByApp && leakingResults.length === 0 && !leakedAnywhere,
    outcome,
    calls: attempts.map((t) => ({ status: t.status, isError: t.isError, result: (t.result ?? "").slice(0, 300) })),
    leakingResults: leakingResults.map((t) => t.name),
    leakedAnywhere,
  };
}
