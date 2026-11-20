// Attachments in the composer (removable, with ingestion and rejection
// states) and in the transcript (read-only; missing content marked). Each is
// a shared Chip; an image chip leads with its thumbnail (the snapshot's own
// bytes as a data URL from the backend, never a remote address).
import { useEffect, useState, type ClipboardEvent } from "react";

import type { AttachmentInfo } from "../../bindings/AttachmentInfo";
import { formatBytes } from "../../lib/format";
import { commands } from "../../lib/ipc";
import { Banner, Button, Chip, FileIcon, StatusText } from "../../ui/components";
import { clipboardHasFiles, clipboardImages, drafts, isLargePaste, useDraft } from "./drafts";

/** "PNG image · 48.2 KiB · 1280×720", "PDF · 1.1 MiB", "Text · 812 B". */
export function attachmentMeta(info: AttachmentInfo): string {
  const kind =
    info.kind === "image"
      ? `${(info.mediaType.split("/")[1] ?? "").replace("jpeg", "jpg").toUpperCase()} image`.trim()
      : info.kind === "pdf"
        ? "PDF"
        : "Text";
  const dimensions = info.width && info.height ? `${info.width}×${info.height}` : null;
  return [kind, formatBytes(info.sizeBytes), dimensions].filter((part): part is string => part !== null).join(" · ");
}

/** Recently used thumbnails (data URLs can be several MB): oldest dropped first. */
const previews = new Map<string, Promise<string | null>>();
const PREVIEW_CACHE = 24;

/** An image attachment's thumbnail (data URL), read once per attachment. */
export function useAttachmentPreview(info: AttachmentInfo): string | null {
  const wanted = info.kind === "image" && !info.missing;
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    if (!wanted) return;
    let live = true;
    let request = previews.get(info.id);
    if (!request) {
      request = Promise.resolve()
        .then(() => commands.agentsAttachmentPreview(info.id))
        .catch(() => null);
      previews.set(info.id, request);
      while (previews.size > PREVIEW_CACHE) previews.delete(previews.keys().next().value as string);
    }
    void request.then((value) => {
      if (live) setUrl(value);
    });
    return () => {
      live = false;
    };
  }, [info.id, wanted]);
  return wanted ? url : null;
}

function AttachmentChip({ info, onRemove }: { info: AttachmentInfo; onRemove?: () => void }) {
  const preview = useAttachmentPreview(info);
  return (
    <Chip
      role="listitem"
      label={info.name}
      thumbnail={
        preview ? (
          <img src={preview} alt="" className="agents-attachment__thumb" data-testid="agents-attachment-thumb" />
        ) : (
          <FileIcon name={info.name} />
        )
      }
      meta={info.missing ? <StatusText status="unavailable" label="Content no longer available" /> : attachmentMeta(info)}
      onRemove={onRemove}
      tone={info.missing ? "warning" : "neutral"}
      data-testid="agents-attachment"
      data-attachment-id={info.id}
      data-kind={info.kind}
      data-missing={info.missing || undefined}
    />
  );
}

/** Attachments sent with a message (transcript). */
export function MessageAttachments({ items }: { items: AttachmentInfo[] }) {
  if (!items.length) return null;
  return (
    <div role="list" aria-label="Attachments" className="agents-attachments" data-testid="agents-message-attachments">
      {items.map((info) => (
        <AttachmentChip key={info.id} info={info} />
      ))}
    </div>
  );
}

/**
 * The latest large paste that became an attachment: what happened, with
 * Insert as text (the text goes into the request instead, when it fits) and
 * Undo (the attachment goes, the text it replaced comes back).
 */
function PasteNotice({ draftKey, sending }: { draftKey: string; sending: boolean }) {
  const draft = useDraft(draftKey);
  const paste = (draft.pastes ?? []).at(-1);
  const info = paste ? draft.attachments.find((a) => a.id === paste.id) : undefined;
  if (!paste || !info || sending) return null;
  const fits = drafts.pasteFitsInline(draftKey, paste.id);
  return (
    <div className="agents-row" role="status" data-testid="agents-paste-notice" data-attachment-id={paste.id}>
      <span className="ui-meta">
        The pasted text was attached as {info.name}
        {fits ? "." : "; it is too long to insert as text."}
      </span>
      {fits && (
        <Button size="sm" variant="ghost" onPress={() => drafts.insertPasteInline(draftKey, paste.id)} data-testid="agents-paste-inline">
          Insert as text
        </Button>
      )}
      <Button size="sm" variant="ghost" onPress={() => drafts.undoPaste(draftKey, paste.id)} data-testid="agents-paste-undo">
        Undo
      </Button>
    </div>
  );
}

/**
 * A composer's attachments: the attached files (each removable), ingestion
 * in progress, and why files were not attached. Attach is in the composer's bar.
 */
export function DraftAttachments({ draftKey, sending = false }: { draftKey: string; sending?: boolean }) {
  const draft = useDraft(draftKey);
  if (draft.ingesting === 0 && draft.rejections.length === 0 && draft.attachments.length === 0) return null;
  return (
    <div className="agents-stack agents-stack--tight" data-testid="agents-draft-attachments" data-ingesting={draft.ingesting > 0 || undefined}>
      {draft.ingesting > 0 && (
        <span role="status">
          <StatusText status="running" label="Adding attachments…" />
        </span>
      )}
      {draft.rejections.length > 0 && (
        <Banner
          tone="warning"
          title={draft.rejections.length === 1 ? "A file was not attached" : `${draft.rejections.length} files were not attached`}
          data-testid="agents-attachment-rejections"
          actions={
            <Button size="sm" variant="ghost" onPress={() => drafts.dismissRejections(draftKey)}>
              Dismiss
            </Button>
          }
        >
          <ul className="agents-list">
            {draft.rejections.map((r) => (
              <li key={r.id} data-testid="agents-attachment-rejection">
                <strong>{r.name}</strong>: {r.message}
              </li>
            ))}
          </ul>
        </Banner>
      )}
      {draft.attachments.length > 0 && (
        <div role="list" aria-label="Attachments" className="agents-attachments" data-testid="agents-draft-attachment-list">
          {draft.attachments.map((info) => (
            <AttachmentChip
              key={info.id}
              info={info}
              // An attachment in a send that is still running cannot be removed.
              onRemove={sending ? undefined : () => drafts.remove(draftKey, info.id)}
            />
          ))}
        </div>
      )}
      <PasteNotice draftKey={draftKey} sending={sending} />
    </div>
  );
}

/**
 * Paste handler for a composer's text field. Image bytes become attachments
 * of that draft, and text beside files or images is left to the field as
 * before (never also attached). A text-only paste is left to the field when
 * it is short; a large one (`isLargePaste`) becomes a text attachment instead
 * of being inserted, and the selection it would have replaced is removed
 * only once that attachment exists.
 */
export function pasteHandler(draftKey: string) {
  return (event: ClipboardEvent<HTMLElement>) => {
    const field = event.target;
    if (!(field instanceof HTMLTextAreaElement)) return;
    const data = event.clipboardData;
    const images = clipboardImages(data);
    if (images.length) void drafts.pasteImages(draftKey, images);
    if (clipboardHasFiles(data)) return;
    const text = data?.getData("text/plain") ?? "";
    if (!isLargePaste(text)) return;
    event.preventDefault();
    void drafts.pasteText(draftKey, text, { value: field.value, start: field.selectionStart, end: field.selectionEnd });
  };
}
