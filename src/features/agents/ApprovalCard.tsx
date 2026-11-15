// One pending approval: what will run or change, complete, with Approve / Decline.
import { Check, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";

import type { ApprovalRequest } from "../../bindings/ApprovalRequest";
import { commands } from "../../lib/ipc";
import { Banner, Button } from "../../ui/components";
import { agents } from "./store";

/** Size of an approval payload, so the user knows how much there is to review. */
export function payloadSize(text: string): string {
  const lines = text.split("\n").length;
  const bytes = new TextEncoder().encode(text).length;
  return `${lines.toLocaleString()} ${lines === 1 ? "line" : "lines"} · ${bytes.toLocaleString()} ${bytes === 1 ? "byte" : "bytes"}`;
}

/**
 * Whether an element's content overflows it (it scrolls). Re-measured when
 * `content` changes and when the element is resized.
 */
function useOverflows(content: string) {
  const ref = useRef<HTMLPreElement>(null);
  const [overflows, setOverflows] = useState(false);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const measure = () => setOverflows(el.scrollHeight > el.clientHeight || el.scrollWidth > el.clientWidth);
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, [content]);
  return [ref, overflows] as const;
}

export function ApprovalCard({ approval }: { approval: ApprovalRequest }) {
  const [busy, setBusy] = useState(false);
  // A payload that scrolls is a tab stop, so it can be read with the keyboard;
  // a short one is not, and Tab goes straight to Approve.
  const [payloadRef, scrolls] = useOverflows(approval.detail);
  async function resolve(approved: boolean) {
    setBusy(true);
    await agents.run(() => commands.agentsApprovalResolve(approval.taskId, approval.id, approved));
    setBusy(false);
  }
  return (
    <section className="agents-stack agents-approval" aria-label={`Approval needed: ${approval.title}`} data-testid="agents-approval">
      <Banner tone="warning" title={approval.title}>
        Nothing is applied until you approve. Review the complete content below.
      </Banner>
      <p className="ui-meta" data-testid="agents-approval-size">
        {payloadSize(approval.detail)} · complete, nothing omitted
      </p>
      <pre
        ref={payloadRef}
        className="agents-pre agents-approval__payload"
        tabIndex={scrolls ? 0 : undefined}
        role="region"
        aria-label={`Complete content to approve: ${approval.title}`}
        data-testid="agents-approval-payload"
      >
        {approval.detail}
      </pre>
      <div className="agents-row">
        <Button size="sm" variant="primary" icon={Check} isDisabled={busy} onPress={() => void resolve(true)} data-testid="agents-approve">
          Approve
        </Button>
        <Button size="sm" variant="secondary" icon={X} isDisabled={busy} onPress={() => void resolve(false)}>
          Decline
        </Button>
      </div>
    </section>
  );
}
