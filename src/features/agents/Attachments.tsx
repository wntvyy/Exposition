// Attachments in the composers (removable, with ingestion and rejection
// states) and in the transcript (read-only; missing content marked). Rows are
// shared ListRows; an image row leads with its thumbnail (the snapshot's own
// bytes as a data URL from the backend, never a remote address).
import { Paperclip, X } from "lucide-react";
import { useEffect, useState, type ClipboardEvent } from "react";

import type { AttachmentInfo } from "../../bindings/AttachmentInfo";
import { commands } from "../../lib/ipc";
import { Banner, Button, FileIcon, IconButton, ListRow, StatusText } from "../../ui/components";
import { clipboardImages, drafts, useDraft } from "./drafts";

/** "12.3 KB" (the backend's units). */
export function humanSize(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${bytes} bytes`;
}

/** "PNG image · 48.2 KB · 1280×720", "PDF · 1.1 MB", "Text · 812 bytes". */
export function attachmentMeta(info: AttachmentInfo): string {
  const kind =
    info.kind === "image"
      ? `${(info.mediaType.split("/")[1] ?? "").replace("jpeg", "jpg").toUpperCase()} image`.trim()
      : info.kind === "pdf"
        ? "PDF"
        : "Text";
  const dimensions = info.width && info.height ? `${info.width}×${info.height}` : null;
  return [kind, humanSize(info.sizeBytes), dimensions].filter((part): part is string => part !== null).join(" · ");
}

const previews = new Map<string, Promise<string | null>>();

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

function AttachmentRow({ info, onRemove }: { info: AttachmentInfo; onRemove?: () => void }) {
  const preview = useAttachmentPreview(info);
  return (
    <ListRow
      role="listitem"
      static
      icon={
        preview ? (
          <img src={preview} alt="" className="agents-attachment__thumb" data-testid="agents-attachment-thumb" />
        ) : (
          <FileIcon name={info.name} />
        )
      }
      title={info.name}
      meta={
        info.missing ? (
          <StatusText status="unavailable" label="Content no longer available" />
        ) : (
          <span>{attachmentMeta(info)}</span>
        )
      }
      trailing={
        onRemove && (
          <IconButton icon={X} size="sm" aria-label={`Remove ${info.name}`} onPress={onRemove} data-testid="agents-attachment-remove" />
        )
      }
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
        <AttachmentRow key={info.id} info={info} />
      ))}
    </div>
  );
}

/**
 * A composer's attachments: the Attach files control, the attached files
 * (each removable), ingestion in progress, and why files were not attached.
 */
export function DraftAttachments({ draftKey }: { draftKey: string }) {
  const draft = useDraft(draftKey);
  return (
    <div className="agents-stack" data-testid="agents-draft-attachments" data-ingesting={draft.ingesting > 0 || undefined}>
      <div className="agents-row">
        <Button
          size="sm"
          variant="secondary"
          icon={Paperclip}
          onPress={() => void drafts.pickFiles(draftKey)}
          data-testid="agents-attach"
        >
          Attach files…
        </Button>
        {draft.ingesting > 0 && (
          <span role="status">
            <StatusText status="running" label="Adding attachments…" />
          </span>
        )}
      </div>
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
            <AttachmentRow key={info.id} info={info} onRemove={() => drafts.remove(draftKey, info.id)} />
          ))}
        </div>
      )}
    </div>
  );
}

/**
 * Paste handler for a composer's text field: image bytes become attachments
 * of that draft; text is left to the field (a mixed paste does both).
 */
export function pasteHandler(draftKey: string) {
  return (event: ClipboardEvent<HTMLElement>) => {
    if (!(event.target instanceof HTMLTextAreaElement)) return;
    const images = clipboardImages(event.clipboardData);
    if (images.length) void drafts.pasteImages(draftKey, images);
  };
}
