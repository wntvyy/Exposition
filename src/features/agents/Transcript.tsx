// Transcript entries of an agent task: messages, reasoning summaries, tool calls, notices, and the streaming draft.
import type { TranscriptItem } from "../../bindings/TranscriptItem";
import { StatusText } from "../../ui/components";
import { activeToolLabel, toolStatusLabel, toolSummary } from "./format";
import type { StreamDraft } from "./store";

export function TranscriptEntry({ item }: { item: TranscriptItem }) {
  switch (item.kind) {
    case "userText":
      return (
        <div className="agents-msg" data-role="user">
          <span className="ui-group-label">You</span>
          <p className="agents-msg__text">{item.text}</p>
        </div>
      );
    case "assistantText":
      return (
        <div className="agents-msg" data-role="assistant" data-testid="agents-assistant-text">
          <span className="ui-group-label">Agent</span>
          <p className="agents-msg__text">{item.text}</p>
        </div>
      );
    case "thinking":
      return (
        <details className="agents-disclosure">
          <summary>Reasoning summary</summary>
          <p className="agents-msg__text ui-meta">{item.text}</p>
        </details>
      );
    case "toolCall": {
      const pill = toolStatusLabel(item.status, item.result);
      return (
        <details className="agents-disclosure agents-tool" data-testid="agents-tool-call" data-tool={item.name}>
          <summary>
            <code className="ui-code">{item.name}</code>
            <span className="ui-meta agents-tool__summary">{toolSummary(item.name, item.input)}</span>
            <StatusText status={pill.status} label={pill.label} />
          </summary>
          <pre className="agents-pre">{JSON.stringify(item.input, null, 2)}</pre>
          {item.result && <pre className="agents-pre">{item.result}</pre>}
        </details>
      );
    }
    case "notice":
      return (
        <p className="agents-notice" data-level={item.level}>
          {item.message}
        </p>
      );
  }
}

export function Draft({ draft }: { draft: StreamDraft }) {
  return (
    <div className="agents-msg" data-role="assistant" data-testid="agents-draft">
      <span className="ui-group-label">Agent</span>
      {draft.thinking && (
        <details className="agents-disclosure">
          <summary>Reasoning summary (streaming)</summary>
          <p className="agents-msg__text ui-meta">{draft.thinking}</p>
        </details>
      )}
      {draft.text && <p className="agents-msg__text">{draft.text}</p>}
      {draft.activeTool && <StatusText {...activeToolLabel(draft.activeTool, draft.activeToolStatus ?? "running")} />}
      {!draft.text && !draft.thinking && !draft.activeTool && <StatusText status="running" label="Thinking" />}
    </div>
  );
}
