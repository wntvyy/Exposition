// Composer drafts, one per conversation: the new-task composer ("new") and
// each task's follow-up (its task id). A draft holds the text, the ingested
// attachments, per-file rejections, ingestion in progress, and an explicit
// model choice (`null` inherits). Async work (file picking, pasted images)
// lands in the draft it started from, even after navigating elsewhere.
//
// Attachments are snapshots made by the backend: files only through its
// native dialog (the webview never passes paths), pasted images as bytes.
// Paste never reads text, HTML, or URLs from the clipboard and never fetches
// anything.
import { create } from "zustand";

import type { AttachmentId } from "../../bindings/AttachmentId";
import type { AttachmentInfo } from "../../bindings/AttachmentInfo";
import type { IngestOutcome } from "../../bindings/IngestOutcome";
import type { ProviderChoice } from "../../bindings/ProviderChoice";
import { commands, errorMessage } from "../../lib/ipc";

/** The new-task composer's draft key (task drafts use the task id). */
export const NEW_TASK_DRAFT = "new";

export interface DraftRejection {
  id: number;
  name: string;
  message: string;
}

export interface ComposerDraft {
  text: string;
  attachments: AttachmentInfo[];
  rejections: DraftRejection[];
  /** Ingestions in progress (sending waits for them). */
  ingesting: number;
  /** Pasted images named so far ("Pasted image 1.png", …). */
  pastedImages: number;
  /** Explicit provider/model for this message; `null` keeps the default (or the task's). */
  model: ProviderChoice | null;
}

export const EMPTY_DRAFT: ComposerDraft = { text: "", attachments: [], rejections: [], ingesting: 0, pastedImages: 0, model: null };

export const useDrafts = create<{ drafts: Record<string, ComposerDraft> }>(() => ({ drafts: {} }));

export function useDraft(key: string): ComposerDraft {
  return useDrafts((s) => s.drafts[key] ?? EMPTY_DRAFT);
}

function get(key: string): ComposerDraft {
  return useDrafts.getState().drafts[key] ?? EMPTY_DRAFT;
}

function update(key: string, change: (draft: ComposerDraft) => Partial<ComposerDraft>): void {
  useDrafts.setState((s) => {
    const draft = s.drafts[key] ?? EMPTY_DRAFT;
    return { drafts: { ...s.drafts, [key]: { ...draft, ...change(draft) } } };
  });
}

let rejectionIds = 0;

function apply(key: string, outcomes: IngestOutcome[]): void {
  const added = outcomes.flatMap((o) => (o.kind === "added" ? [o.attachment] : []));
  const rejected = outcomes.flatMap((o) => (o.kind === "rejected" ? [{ id: ++rejectionIds, name: o.name, message: o.message }] : []));
  if (!added.length && !rejected.length) return;
  update(key, (d) => ({ attachments: [...d.attachments, ...added], rejections: [...d.rejections, ...rejected] }));
}

function reject(key: string, name: string, message: string): void {
  update(key, (d) => ({ rejections: [...d.rejections, { id: ++rejectionIds, name, message }] }));
}

async function ingesting<T>(key: string, work: () => Promise<T>): Promise<T> {
  update(key, (d) => ({ ingesting: d.ingesting + 1 }));
  try {
    return await work();
  } finally {
    update(key, (d) => ({ ingesting: Math.max(0, d.ingesting - 1) }));
  }
}

// ---------------------------------------------------------------- clipboard

const IMAGE_EXTENSIONS: Record<string, string> = {
  "image/png": "png",
  "image/jpeg": "jpg",
  "image/gif": "gif",
  "image/webp": "webp",
  "image/bmp": "bmp",
};

/**
 * Image files in a paste. The same image is often offered twice (as a file
 * item and in the file list): the items are used when they hold any image,
 * the file list only otherwise. Text, HTML, and URLs are ignored here (the
 * textarea inserts text itself), and nothing is ever fetched.
 */
export function clipboardImages(data: DataTransfer | null): File[] {
  if (!data) return [];
  const fromItems = Array.from(data.items ?? [])
    .filter((item) => item.kind === "file" && item.type.startsWith("image/"))
    .map((item) => item.getAsFile())
    .filter((file): file is File => file !== null);
  if (fromItems.length) return fromItems;
  return Array.from(data.files ?? []).filter((file) => file.type.startsWith("image/"));
}

/** Browsers name a pasted bitmap "image.png": that says nothing, so it is renamed. */
export function genericPasteName(name: string): boolean {
  return !name.trim() || /^image\.[a-z0-9]+$/i.test(name.trim());
}

function readBase64(file: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const url = String(reader.result ?? "");
      const comma = url.indexOf(",");
      resolve(comma >= 0 ? url.slice(comma + 1) : url);
    };
    reader.onerror = () => reject(reader.error ?? new Error("read failed"));
    reader.readAsDataURL(file);
  });
}

// ---------------------------------------------------------------- actions

export const drafts = {
  get,

  setText(key: string, text: string) {
    update(key, () => ({ text }));
  },

  setModel(key: string, model: ProviderChoice | null) {
    update(key, () => ({ model }));
  },

  /** Open the backend's file dialog; keep what was added, explain each rejected file. */
  async pickFiles(key: string): Promise<void> {
    await ingesting(key, async () => {
      try {
        apply(key, await commands.agentsAttachmentsPick());
      } catch (e) {
        reject(key, "Attach files", errorMessage(e));
      }
    });
  },

  /** Attach pasted images (each one distinct; repeated bytes in one paste once). */
  async pasteImages(key: string, files: File[]): Promise<void> {
    if (!files.length) return;
    await ingesting(key, async () => {
      const seen = new Set<string>();
      for (const file of files) {
        let data: string;
        try {
          data = await readBase64(file);
        } catch {
          reject(key, file.name || "Pasted image", "The pasted image could not be read.");
          continue;
        }
        // One image offered in two representations of the same bytes.
        if (seen.has(data)) continue;
        seen.add(data);
        let name = file.name;
        if (genericPasteName(name)) {
          const n = get(key).pastedImages + 1;
          update(key, () => ({ pastedImages: n }));
          name = `Pasted image ${n}.${IMAGE_EXTENSIONS[file.type] ?? "png"}`;
        }
        try {
          apply(key, [await commands.agentsAttachmentsAddBytes(name, file.type || null, data)]);
        } catch (e) {
          reject(key, name, errorMessage(e));
        }
      }
    });
  },

  /** Remove a draft attachment (the backend forgets it unless a message uses it). */
  remove(key: string, id: AttachmentId) {
    update(key, (d) => ({ attachments: d.attachments.filter((a) => a.id !== id) }));
    void Promise.resolve()
      .then(() => commands.agentsAttachmentsDiscard([id]))
      .catch(() => {});
  },

  dismissRejections(key: string) {
    update(key, () => ({ rejections: [] }));
  },

  /**
   * After a successful send, clear only what was sent: text typed and
   * attachments added while the request ran stay in the draft.
   */
  completeSend(key: string, sent: { text: string; attachments: AttachmentId[]; model: ProviderChoice | null }) {
    update(key, (d) => ({
      text: d.text === sent.text ? "" : d.text.startsWith(sent.text) ? d.text.slice(sent.text.length).replace(/^\s+/, "") : d.text,
      attachments: d.attachments.filter((a) => !sent.attachments.includes(a.id)),
      rejections: [],
      model: d.model === sent.model ? null : d.model,
    }));
  },

  /** Tests only. */
  resetForTests() {
    useDrafts.setState({ drafts: {} });
  },
};
