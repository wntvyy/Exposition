// V2 review samples: the compact Agent panel, design C structure unchanged —
// task header with one status pill, a turn outline, the inline approval card
// with its complete payload, a question card, Needs you, and the composer with
// the Options button. Mock data; nothing is sent or approved.
import {
  ArrowLeft,
  ArrowUp,
  Check,
  CheckCheck,
  ChevronDown,
  ChevronRight,
  CircleHelp,
  ListTree,
  MessageSquare,
  MoreHorizontal,
  Network,
  Paperclip,
  Send,
  ShieldAlert,
  SlidersHorizontal,
  Sigma,
  Square,
  X,
} from "lucide-react";
import { useEffect, useRef, useState } from "react";

import { Badge, Banner, Btn, ChromeLabel, StatusPill } from "./kit";
import { AGENT, payloadSize } from "./mock";

export function AgentPanel({ scrollTo }: { scrollTo?: "question" | null }) {
  const [turn1Open, setTurn1Open] = useState(false);
  const [turn2Open, setTurn2Open] = useState(true);
  const [answer, setAnswer] = useState<string | null>(null);
  const logRef = useRef<HTMLDivElement>(null);
  const approvalRef = useRef<HTMLElement>(null);
  const questionRef = useRef<HTMLElement>(null);

  const jump = (target: "approval" | "question") => {
    const el = (target === "approval" ? approvalRef : questionRef).current;
    const log = logRef.current;
    if (el && log) log.scrollTop = el.offsetTop - log.offsetTop - 8;
  };

  // Open at the action point, as a Needs you jump would: the approval card.
  useEffect(() => {
    jump(scrollTo === "question" ? "question" : "approval");
  }, [scrollTo]);

  return (
    <div className="v2-agent" data-testid="v2-agent-panel">
      <div className="v2-pane-header v2-pane-header--tabs">
        <span className="v2-pane-tab" aria-selected="true" role="tab">
          <ChromeLabel>Agent</ChromeLabel>
        </span>
        <Btn variant="ghost" size="sm" icon={Sigma} label="Mathematics" />
        <Btn variant="ghost" size="sm" icon={ListTree} label="Outline" />
        <Btn variant="ghost" size="sm" icon={Network} label="Graph context" />
        <span className="v2-spacer" />
        <Btn variant="ghost" size="sm" icon={X} label="Close secondary sidebar" />
      </div>

      <div className="v2-agent__task">
        <Btn variant="ghost" size="sm" icon={ArrowLeft} label="Back to tasks" />
        <h2 className="v2-agent__title" title={AGENT.title}>
          {AGENT.title}
        </h2>
        <StatusPill tone="warning">Waiting for approval</StatusPill>
        <Btn variant="ghost" size="sm" icon={MoreHorizontal} label="Task actions" />
      </div>

      <div className="v2-agent__log" role="log" aria-label="Conversation" ref={logRef} tabIndex={0}>
        <div className="v2-agent__you">
          <div className="v2-group-label">You</div>
          <p className="v2-prose">{AGENT.prompt}</p>
        </div>

        <TurnHeader open={turn1Open} onToggle={() => setTurn1Open((v) => !v)} summary={AGENT.turn1Summary} n={1} />
        {turn1Open && (
          <div className="v2-turn__body">
            <Response text={AGENT.turn1Text} />
          </div>
        )}

        <TurnHeader open={turn2Open} onToggle={() => setTurn2Open((v) => !v)} summary={AGENT.turn2Summary} n={2} waiting />
        {turn2Open && (
          <div className="v2-turn__body">
            <Response text={AGENT.turn2Text} />

            <section className="v2-approval" aria-label={`Approval needed: ${AGENT.approval.title}`} ref={approvalRef} data-testid="v2-approval">
              <Banner tone="warning" title={AGENT.approval.title}>
                Nothing is applied until you approve. Review the complete content below.
              </Banner>
              <p className="v2-meta">{payloadSize(AGENT.approval.payload)} · complete, nothing omitted</p>
              <pre className="v2-payload" role="region" aria-label={`Complete content to approve: ${AGENT.approval.title}`} data-testid="v2-approval-payload">
                {AGENT.approval.payload}
              </pre>
              <div className="v2-row">
                <Btn variant="primary" size="sm" icon={Check}>
                  Approve
                </Btn>
                <Btn size="sm" icon={CheckCheck}>
                  Approve for this task
                </Btn>
                <Btn size="sm" icon={X}>
                  Decline
                </Btn>
              </div>
              <p className="v2-meta">{AGENT.approval.scope}</p>
            </section>

            <section className="v2-question" aria-label="Question from the agent" ref={questionRef} data-testid="v2-question">
              <Banner tone="info" title="Needs your answer">
                {AGENT.question.banner}
              </Banner>
              <p className="v2-meta">{AGENT.question.progress}</p>
              <fieldset className="v2-options">
                <legend className="v2-question__text">{AGENT.question.text}</legend>
                <p className="v2-question__detail">{AGENT.question.detail}</p>
                {AGENT.question.options.map((o) => (
                  <label key={o.id} className="v2-option" data-checked={answer === o.id ? "true" : undefined}>
                    <input type="radio" name="v2-unit" value={o.id} checked={answer === o.id} onChange={() => setAnswer(o.id)} />
                    <span className="v2-option__body">
                      <span className="v2-option__label">
                        {o.label}
                        {o.recommended && (
                          <Badge tone="accent" dashed>
                            Recommended
                          </Badge>
                        )}
                      </span>
                      <span className="v2-option__desc">{o.description}</span>
                    </span>
                  </label>
                ))}
                <input className="v2-input" placeholder="Other answer…" aria-label="Other answer" />
              </fieldset>
              <div className="v2-row">
                <Btn size="sm" icon={Send} disabled={!answer}>
                  Send answer
                </Btn>
                <Btn size="sm" icon={MessageSquare}>
                  Chat about it
                </Btn>
              </div>
            </section>
          </div>
        )}
      </div>

      <nav className="v2-needs" aria-label="Needs you">
        <div className="v2-group-label">
          Needs you <span className="v2-count">2</span>
        </div>
        <button type="button" className="v2-needs__item" onClick={() => jump("approval")}>
          <ShieldAlert size={14} aria-hidden className="v2-needs__icon" data-tone="warning" />
          <span className="v2-needs__text">Waiting for approval: {AGENT.approval.title}</span>
          <span className="v2-needs__jump">
            Jump <ArrowUp size={12} aria-hidden />
          </span>
        </button>
        <button type="button" className="v2-needs__item" onClick={() => jump("question")}>
          <CircleHelp size={14} aria-hidden className="v2-needs__icon" data-tone="info" />
          <span className="v2-needs__text">Needs your answer: {AGENT.question.text}</span>
          <span className="v2-needs__jump">
            Jump <ArrowUp size={12} aria-hidden />
          </span>
        </button>
      </nav>

      <div className="v2-composer" data-testid="v2-composer">
        <div className="v2-meta v2-composer__context">{AGENT.context}</div>
        <textarea className="v2-input v2-composer__field" placeholder="Reply or give the next instruction" aria-label="Message" rows={2} />
        <div className="v2-composer__options">
          <Btn variant="ghost" size="sm" icon={SlidersHorizontal} expanded={false}>
            <span className="v2-truncate">{AGENT.options}</span>
          </Btn>
          <Badge tone="warning">Native</Badge>
        </div>
        <div className="v2-row v2-composer__actions">
          <Btn variant="ghost" size="sm" icon={Paperclip} label="Attach files" />
          <span className="v2-spacer" />
          <Btn size="sm" icon={Square}>
            Stop
          </Btn>
          <Btn size="sm" icon={ArrowUp} disabled>
            Send
          </Btn>
        </div>
        <p className="v2-meta">Answer the question above, or use Chat about it.</p>
      </div>
    </div>
  );
}

function TurnHeader({ open, onToggle, summary, n, waiting }: { open: boolean; onToggle: () => void; summary: string; n: number; waiting?: boolean }) {
  return (
    <button type="button" className="v2-turn" aria-expanded={open} onClick={onToggle} data-waiting={waiting ? "true" : undefined}>
      {open ? <ChevronDown size={14} aria-hidden /> : <ChevronRight size={14} aria-hidden />}
      <span className="v2-turn__label">Turn {n}</span>
      <span className="v2-turn__summary">{summary}</span>
    </button>
  );
}

function Response({ text }: { text: string }) {
  return (
    <div className="v2-response">
      <div className="v2-group-label">Agent</div>
      <p className="v2-prose">{text}</p>
    </div>
  );
}
