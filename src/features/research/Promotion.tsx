// Retention (keep / archive / discard) and reviewed promotion of a run's
// figure or table into a manuscript. Promotion wraps the format-owned
// attachment path: the person reviews the exact markup, the target document
// revision, and the artifact digest, then approves; approval is refused if
// either changed since the review.
import { Archive, BookmarkCheck, Check, FileInput, Trash2 } from "lucide-react";
import { useEffect, useState } from "react";

import { notify } from "../../app/notifications";
import type { ArtifactRecord } from "../../bindings/ArtifactRecord";
import type { PromotionOutcome } from "../../bindings/PromotionOutcome";
import type { PromotionPreview } from "../../bindings/PromotionPreview";
import type { RetainedResult } from "../../bindings/RetainedResult";
import type { RunSummary } from "../../bindings/RunSummary";
import { commands, errorMessage, isCommandError } from "../../lib/ipc";
import { graphCommands } from "../../lib/graphIpc";
import { Banner, Button, Select, Spinner, StatusPill, TextField } from "../../ui/components";
import { openDocument } from "../editor/documentClient";
import { researchCommands } from "./ipc";
import { attachmentsOfKind, defaultInsertOffset, documentFormat, formatBytes } from "./model";
import {
  reconcilePromotion,
  refreshListing,
  refreshPromotions,
  resultForPath,
  retainArtifact,
  setResultState,
  useSpace,
} from "./space";

export function lifecyclePill(result: RetainedResult | undefined): {
  status: "idle" | "succeeded" | "canceled" | "stale";
  label: string;
} {
  if (!result) return { status: "idle", label: "Not retained" };
  switch (result.lifecycle) {
    case "active":
      return { status: "succeeded", label: "Kept" };
    case "archived":
      return { status: "canceled", label: "Archived" };
    case "discarded":
      return { status: "canceled", label: "Discarded" };
    default:
      return { status: "stale", label: "Missing" };
  }
}

/** Keep / Archive / Discard for one artifact of a run (operational output until kept). */
export function RetainControls({ run, artifact }: { run: RunSummary; artifact: ArtifactRecord }) {
  const result = useSpace((s) => resultForPath(s.listing, artifact.path));
  const [busy, setBusy] = useState(false);
  const pill = lifecyclePill(result);
  const finished = run.status.kind !== "running";
  const act = async (fn: () => Promise<boolean>) => {
    setBusy(true);
    await fn();
    setBusy(false);
  };
  return (
    <div
      className="exp-research__retain"
      role="group"
      aria-label="Retain this result"
      data-testid="research-retain"
      data-state={result?.lifecycle ?? "none"}
    >
      <StatusPill size="sm" status={pill.status} label={pill.label} />
      {result?.lifecycle !== "active" && (
        <Button
          size="sm"
          variant="ghost"
          icon={BookmarkCheck}
          isDisabled={!finished || busy}
          onPress={() =>
            void act(() => (result ? setResultState(result.entity, "active") : retainArtifact(run, artifact, "active")))
          }
        >
          Keep
        </Button>
      )}
      {result?.lifecycle !== "archived" && (
        <Button
          size="sm"
          variant="ghost"
          icon={Archive}
          isDisabled={!finished || busy}
          onPress={() =>
            void act(() => (result ? setResultState(result.entity, "archived") : retainArtifact(run, artifact, "archived")))
          }
        >
          Archive
        </Button>
      )}
      {result && result.lifecycle !== "discarded" && (
        <Button
          size="sm"
          variant="ghost"
          icon={Trash2}
          isDisabled={busy}
          onPress={() => void act(() => setResultState(result.entity, "discarded"))}
        >
          Discard
        </Button>
      )}
    </div>
  );
}

interface Target {
  key: string;
  path: string;
  document: string | null;
}

/** Open Typst/LaTeX/Markdown documents, then the project's other manuscripts. */
export async function promotionTargets(): Promise<Target[]> {
  const targets: Target[] = [];
  const docs = await commands.documentList().catch(() => []);
  for (const d of docs) {
    if (d.path && documentFormat(d.path) && !d.readOnly) targets.push({ key: d.path, path: d.path, document: d.id });
  }
  targets.sort((a, b) => a.path.localeCompare(b.path));
  try {
    const snapshot = await graphCommands.snapshot({
      kinds: ["resource"],
      text: null,
      around: null,
      includeInactive: false,
      limit: 500,
    });
    const more: Target[] = [];
    for (const e of snapshot.entities) {
      const path = e.key;
      if (!path || !e.roles.includes("manuscript") || !documentFormat(path) || targets.some((t) => t.path === path))
        continue;
      more.push({ key: path, path, document: null });
    }
    more.sort((a, b) => a.path.localeCompare(b.path));
    targets.push(...more);
  } catch {
    // The graph is optional here: open documents are still offered.
  }
  return targets;
}

type Step =
  | { kind: "choose" }
  | { kind: "preparing" }
  | { kind: "review"; preview: PromotionPreview }
  | { kind: "applying"; preview: PromotionPreview }
  | { kind: "pending"; outcome: PromotionOutcome };

interface Failure {
  code: string;
  message: string;
}

/**
 * "Insert or update in document…": choose the document and placement, review
 * the exact markup, then approve. Inline in the output, next to the result.
 */
export function PromotionControls({ run, artifact }: { run: RunSummary; artifact: ArtifactRecord }) {
  const [open, setOpen] = useState(false);
  const finished = run.status.kind !== "running";
  if (!open) {
    return (
      <div className="exp-research__attach">
        <Button size="sm" variant="ghost" icon={FileInput} isDisabled={!finished} onPress={() => setOpen(true)}>
          Insert or update in document…
        </Button>
      </div>
    );
  }
  return <PromotionForm run={run} artifact={artifact} onDone={() => setOpen(false)} />;
}

function PromotionForm({ run, artifact, onDone }: { run: RunSummary; artifact: ArtifactRecord; onDone: () => void }) {
  const [targets, setTargets] = useState<Target[] | null>(null);
  const [target, setTarget] = useState<string | null>(null);
  const [keys, setKeys] = useState<string[]>([]);
  const [key, setKey] = useState<string | null>(null);
  const [caption, setCaption] = useState("");
  const [step, setStep] = useState<Step>({ kind: "choose" });
  const [failure, setFailure] = useState<Failure | null>(null);
  const [lastPlacement, setLastPlacement] = useState<{ kind: "insert" } | { kind: "update"; key: string } | null>(null);
  const what = artifact.kind === "table" ? "table" : "figure";

  useEffect(() => {
    let live = true;
    void promotionTargets().then((list) => {
      if (!live) return;
      setTargets(list);
      setTarget((t) => t ?? list[0]?.key ?? null);
    });
    return () => {
      live = false;
    };
  }, []);

  const selected = targets?.find((t) => t.key === target) ?? null;

  // Existing attachments of this kind in the target (a table never replaces a figure).
  useEffect(() => {
    if (!selected?.document) {
      setKeys([]);
      return;
    }
    let live = true;
    const format = documentFormat(selected.path);
    commands.documentState(selected.document).then(
      (state) => {
        if (!live || !format) return;
        const found = attachmentsOfKind(state.text, format, what);
        setKeys(found);
        setKey((current) => (current && found.includes(current) ? current : (found[0] ?? null)));
      },
      () => live && setKeys([]),
    );
    return () => {
      live = false;
    };
  }, [selected, what, step.kind]);

  const prepare = async (placementChoice: { kind: "insert" } | { kind: "update"; key: string }) => {
    if (!selected) return;
    setFailure(null);
    setLastPlacement(placementChoice);
    setStep({ kind: "preparing" });
    try {
      const document = selected.document ?? (await openDocument(selected.path)).id;
      const format = documentFormat(selected.path) ?? "markdown";
      const state = await commands.documentState(document);
      const placement =
        placementChoice.kind === "insert"
          ? ({ kind: "insert", offset: defaultInsertOffset(state.text, format) } as const)
          : ({ kind: "update", attachmentKey: placementChoice.key } as const);
      const preview = await researchCommands.preparePromotion({
        runId: run.id,
        artifactId: artifact.id,
        document,
        baseRevision: state.info.revision,
        placement,
        caption: caption.trim() ? caption.trim() : null,
        attachmentKey: null,
      });
      setStep({ kind: "review", preview });
    } catch (e) {
      setFailure({ code: isCommandError(e) ? e.code : "error", message: errorMessage(e) });
      setStep({ kind: "choose" });
    }
  };

  const cancel = () => {
    if (step.kind === "review") void researchCommands.cancelPromotion(step.preview.promotion);
    setFailure(null);
    setStep({ kind: "choose" });
  };

  const approve = async (preview: PromotionPreview) => {
    setFailure(null);
    setStep({ kind: "applying", preview });
    try {
      const outcome = await researchCommands.approvePromotion(preview.promotion);
      for (const warning of outcome.warnings) notify({ tone: "warning", title: "Check the document", body: warning });
      await Promise.all([refreshListing(), refreshPromotions()]);
      if (outcome.graph.kind === "recorded") {
        notify({
          tone: "success",
          title: `${what === "table" ? "Table" : "Figure"} ${preview.update ? "updated in" : "inserted into"} ${outcome.path}`,
          body: `Attachment ${outcome.attachmentKey} (revision ${outcome.revision}), recorded in the project graph. Undo is available in the document.`,
        });
        setStep({ kind: "choose" });
      } else {
        notify({
          id: `promotion-${outcome.operation}`,
          tone: "warning",
          title: "Inserted, but the project graph was not updated",
          body: outcome.graph.reason,
          timeoutMs: null,
          actions: [{ label: "Reconcile", run: () => void reconcilePromotion(outcome.operation) }],
        });
        setStep({ kind: "pending", outcome });
      }
    } catch (e) {
      setFailure({ code: isCommandError(e) ? e.code : "error", message: errorMessage(e) });
      setStep({ kind: "review", preview });
    }
  };

  const reviewAgain =
    failure && ["promotion.stale", "promotion.planChanged", "promotion.unknown", "promotion.expired"].includes(failure.code);

  return (
    <div
      className="exp-research__attach"
      data-testid="research-attach"
      data-step={step.kind}
      role="group"
      aria-label={`Insert this ${what} into a document`}
    >
      {failure && (
        <div data-testid="research-promotion-error" data-code={failure.code}>
          <Banner
            tone={failure.code === "promotion.stale" ? "stale" : "danger"}
            title={
              failure.code === "promotion.stale"
                ? "The document changed after your review"
                : failure.code === "research.artifactChanged"
                  ? "The artifact changed since its run recorded it"
                  : "Not inserted"
            }
            actions={
              reviewAgain && lastPlacement ? (
                <Button size="sm" onPress={() => void prepare(lastPlacement)}>
                  Review again
                </Button>
              ) : undefined
            }
          >
            {failure.message}
          </Banner>
        </div>
      )}
      {(step.kind === "choose" || step.kind === "preparing") &&
        (targets === null ? (
          <Spinner size="sm" aria-label="Loading documents" />
        ) : targets.length === 0 ? (
          <p className="exp-muted">
            The project has no Typst, LaTeX, or Markdown manuscript to insert this {what} into. Promotion is optional: kept
            results stay in Research.
          </p>
        ) : (
          <>
            <Select<string>
              label="Document"
              options={targets.map((t) => ({ id: t.key, label: t.document ? t.path : `${t.path} (opens it)` }))}
              value={target}
              onChange={setTarget}
            />
            <TextField
              label="Caption"
              value={caption}
              onChange={setCaption}
              placeholder={keys.length ? "Leave empty to keep the existing caption on update" : `Caption for the ${what}`}
            />
            <div className="exp-research__attach-actions">
              <Button
                size="sm"
                variant="primary"
                isPending={step.kind === "preparing"}
                onPress={() => void prepare({ kind: "insert" })}
              >
                Insert into document
              </Button>
              {keys.length > 0 && (
                <>
                  <Select<string>
                    aria-label="Attachment to update"
                    options={keys.map((k) => ({ id: k, label: k }))}
                    value={key}
                    onChange={setKey}
                  />
                  <Button
                    size="sm"
                    isPending={step.kind === "preparing"}
                    isDisabled={!key}
                    onPress={() => key && void prepare({ kind: "update", key })}
                  >
                    Update in document
                  </Button>
                </>
              )}
              <Button size="sm" variant="ghost" onPress={onDone}>
                Close
              </Button>
            </div>
          </>
        ))}
      {(step.kind === "review" || step.kind === "applying") && (
        <>
          <PreviewView preview={step.preview} />
          <div className="exp-research__attach-actions">
            <Button size="sm" variant="secondary" onPress={cancel}>
              Cancel
            </Button>
            {!reviewAgain && (
              <Button
                size="sm"
                variant="primary"
                icon={Check}
                isPending={step.kind === "applying"}
                isDisabled={failure?.code === "research.artifactChanged"}
                onPress={() => void approve(step.preview)}
              >
                {step.preview.update ? "Approve update" : "Approve insertion"}
              </Button>
            )}
          </div>
        </>
      )}
      {step.kind === "pending" && (
        <div data-testid="research-promotion-pending" data-operation={step.outcome.operation}>
          <Banner
            tone="warning"
            title="Inserted, but the project graph was not updated"
            actions={
              <Button
                size="sm"
                variant="primary"
                onPress={() => void reconcilePromotion(step.outcome.operation).then(() => setStep({ kind: "choose" }))}
              >
                Reconcile now
              </Button>
            }
          >
            {step.outcome.graph.kind === "pendingReconciliation" ? step.outcome.graph.reason : null} A pending record was
            kept in the operation journal; reconciling records the attachment without changing the document again.
          </Banner>
        </div>
      )}
    </div>
  );
}

function PreviewView({ preview }: { preview: PromotionPreview }) {
  const a = preview.artifact;
  const digest = a.contentHash.replace(/^sha256:/, "");
  return (
    <div
      className="exp-promotion"
      data-testid="research-promotion-review"
      data-revision={preview.revision}
      data-key={preview.attachmentKey}
      data-path={preview.path}
    >
      <dl className="exp-research__facts">
        <dt>Document</dt>
        <dd>
          <code data-testid="research-promotion-target">{preview.path}</code> · {preview.format} · revision{" "}
          <span data-testid="research-promotion-revision">{preview.revision}</span>
        </dd>
        <dt>Artifact</dt>
        <dd>
          <code>{a.path}</code> · {a.mediaType} · {formatBytes(a.sizeBytes)} · run {preview.runId}
        </dd>
        <dt>Digest</dt>
        <dd>
          <code title={digest} data-testid="research-promotion-digest">
            sha256 {digest}
          </code>{" "}
          · verified now and again on approval
        </dd>
        <dt>Attachment</dt>
        <dd>
          <code>{preview.attachmentKey}</code> · {preview.retained ? "already a kept result" : "approving keeps the result"}
        </dd>
      </dl>
      <p className="exp-muted">The format adapter makes exactly these changes, as one undoable change:</p>
      {preview.edits.map((e, i) => (
        <div key={i} className="exp-promotion__edit">
          <div className="exp-promotion__where">{e.replaced ? `Replace from line ${e.line}` : `Insert at line ${e.line}`}</div>
          {e.replaced && (
            <pre className="exp-promotion__markup exp-promotion__markup--old" aria-label="Replaced text">
              {e.replaced}
            </pre>
          )}
          <pre className="exp-promotion__markup" data-testid="research-promotion-markup" aria-label="Inserted text">
            {e.text}
          </pre>
        </div>
      ))}
      {preview.warnings.map((w) => (
        <Banner key={w} tone="warning" title="Check the document">
          {w}
        </Banner>
      ))}
    </div>
  );
}
