// Composer drafts, one per conversation: the new-task composer ("new") and
// each task's follow-up (its task id). A draft holds the text, the ingested
// attachments, per-file rejections, ingestion in progress, and an explicit
// model choice (`null` inherits). Async work (file picking, dropped files,
// pasted images and text) lands in the draft it started from, even after
// navigating elsewhere.
//
// Attachments are snapshots made by the backend: files through its native
// dialog or a native drop it recorded itself (the webview alone never names a
// file to read), pasted images and large pasted text as bytes. A large text
// paste becomes an ordinary text attachment ("pasted-text-N.txt", the exact
// clipboard text as UTF-8); a short one is left to the field. Paste never
// reads HTML or URLs from the clipboard and never fetches anything.
import { create } from "zustand";

import type { AttachmentId } from "../../bindings/AttachmentId";
import type { AttachmentInfo } from "../../bindings/AttachmentInfo";
import type { IngestOutcome } from "../../bindings/IngestOutcome";
import type { ProviderChoice } from "../../bindings/ProviderChoice";
import { commands, errorMessage } from "../../lib/ipc";

/** The new-task composer's draft key (task drafts use the task id). */
export const NEW_TASK_DRAFT = "new";

// ---------------------------------------------------------------- limits

/**
 * A text paste larger than this becomes an attachment instead of inline text:
 * more than 8,000 characters (UTF-16 code units, as the field counts them) or
 * more than 100 lines. A product threshold for readability of the request,
 * well inside the limits below; it bypasses none of them (a pasted
 * attachment is checked like any text file: at most 1 MiB, and at most
 * `MAX_ATTACHMENTS_PER_MESSAGE` attachments per message).
 */
export const LARGE_PASTE_CHARS = 8_000;
export const LARGE_PASTE_LINES = 100;

/** Attachments one message can carry (the backend's `MAX_PER_MESSAGE`). */
export const MAX_ATTACHMENTS_PER_MESSAGE = 20;

/** Longest request text (characters; the backend's `MAX_PROMPT_CHARS`). */
export const MAX_REQUEST_CHARS = 100_000;

/** Files taken from one drop (the backend's `MAX_DROP_FILES`). */
export const MAX_DROP_FILES = 100;

/** Lines in a text: a final line break does not start another line. */
export function lineCount(text: string): number {
  if (!text) return 0;
  const breaks = text.match(/\r\n|\r|\n/g)?.length ?? 0;
  return /[\r\n]$/.test(text) ? breaks : breaks + 1;
}

/** Whether a pasted text is attached instead of inserted (see `LARGE_PASTE_CHARS`). */
export function isLargePaste(text: string): boolean {
  return text.length > LARGE_PASTE_CHARS || lineCount(text) > LARGE_PASTE_LINES;
}

/**
 * A large paste that became an attachment, kept while that attachment is in
 * the draft so the conversion can be undone or the text inserted instead.
 */
export interface PastedText {
  /** The attachment made from the paste. */
  id: AttachmentId;
  /** The exact clipboard text. */
  text: string;
  /** The draft text before the caret/selection when it was pasted. */
  before: string;
  /** The selected draft text the paste replaced ("" when none). */
  replaced: string;
}

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
  /** Pasted texts named so far ("pasted-text-1.txt", …). */
  pastedTexts?: number;
  /** Large pastes attached and still in the draft (newest last). */
  pastes?: PastedText[];
  /** Explicit provider/model for this message; `null` keeps the default (or the task's). */
  model: ProviderChoice | null;
  /**
   * "Chat about it": the structured question the next message discusses
   * (absent or `null`: an ordinary message). See `drafts.chatAbout`.
   */
  about?: DraftAbout | null;
}

/** The question a draft is about ("About: <prompt>" in the composer). */
export interface DraftAbout {
  /** The question request's id (`QuestionRequest.id`). */
  requestId: string;
  /** Its first prompt, for the composer's About chip. */
  prompt: string;
  /**
   * The draft text when the discussion began. It is unrelated to the
   * question and comes back after the discussion if the field was cleared
   * to discuss (see `sendDiscussion` and `drafts.clearAbout`).
   */
  previousText: string;
  /** Increases with every Chat about it: the composer focuses its field when it changes. */
  seq: number;
}

export const EMPTY_DRAFT: ComposerDraft = { text: "", attachments: [], rejections: [], ingesting: 0, pastedImages: 0, model: null };

export const useDrafts = create<{ drafts: Record<string, ComposerDraft> }>(() => ({ drafts: {} }));

export function useDraft(key: string): ComposerDraft {
  return useDrafts((s) => s.drafts[key] ?? EMPTY_DRAFT);
}

/** The question a conversation's draft is about, if any (Chat about it). */
export function useDraftAbout(key: string): DraftAbout | null {
  return useDrafts((s) => s.drafts[key]?.about ?? null);
}

let aboutSeq = 0;

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

const TOO_MANY = `A message can carry at most ${MAX_ATTACHMENTS_PER_MESSAGE} attachments.`;

function discard(ids: AttachmentId[]): void {
  if (!ids.length) return;
  void Promise.resolve()
    .then(() => commands.agentsAttachmentsDiscard(ids))
    .catch(() => {});
}

/**
 * Put ingestion results into the draft: what was added (up to the
 * per-message limit; with `distinct`, content already in the draft is not
 * added twice) and why each other file was not. Returns what was added.
 */
function apply(key: string, outcomes: IngestOutcome[], options: { distinct?: boolean } = {}): AttachmentInfo[] {
  const current = get(key).attachments;
  const added: AttachmentInfo[] = [];
  const rejected: DraftRejection[] = [];
  const dropped: AttachmentId[] = [];
  for (const outcome of outcomes) {
    if (outcome.kind === "rejected") {
      rejected.push({ id: ++rejectionIds, name: outcome.name, message: outcome.message });
      continue;
    }
    const { attachment } = outcome;
    const twin = options.distinct ? [...current, ...added].find((a) => a.sha256 === attachment.sha256) : undefined;
    const refusal = twin
      ? twin.name === attachment.name
        ? "Already attached."
        : `Already attached as “${twin.name}” (the same content).`
      : current.length + added.length >= MAX_ATTACHMENTS_PER_MESSAGE
        ? `${TOO_MANY} Remove one to attach this file.`
        : null;
    if (refusal) {
      rejected.push({ id: ++rejectionIds, name: attachment.name, message: refusal });
      dropped.push(attachment.id);
    } else {
      added.push(attachment);
    }
  }
  discard(dropped);
  if (added.length || rejected.length) {
    update(key, (d) => ({ attachments: [...d.attachments, ...added], rejections: [...d.rejections, ...rejected] }));
  }
  return added;
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

/** Whether a paste carries files (copied files or images), not only text. */
export function clipboardHasFiles(data: DataTransfer | null): boolean {
  if (!data) return false;
  return (data.files?.length ?? 0) > 0 || Array.from(data.items ?? []).some((item) => item.kind === "file");
}

/** Base64 of a text's UTF-8 bytes (no byte-order mark, nothing normalized). */
export function utf8Base64(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  for (let i = 0; i < bytes.length; i += 0x8000) binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(binary);
}

/** Characters as the backend counts them (code points). */
function characters(text: string): number {
  let n = 0;
  for (const _ of text) n++;
  return n;
}

/** Where a paste's text belongs in the current draft text: its old place when the text before it is unchanged, else the end. */
function pasteOffset(text: string, paste: PastedText): number {
  return text.startsWith(paste.before) ? paste.before.length : text.length;
}

const insertAt = (text: string, offset: number, inserted: string) => text.slice(0, offset) + inserted + text.slice(offset);

/** Remove a pasted-text attachment and its record; `text` replaces the draft text. */
function unpaste(key: string, paste: PastedText, text: string): void {
  update(key, (d) => ({
    text,
    attachments: d.attachments.filter((a) => a.id !== paste.id),
    pastes: (d.pastes ?? []).filter((p) => p.id !== paste.id),
  }));
  discard([paste.id]);
}

const PASTE_KEPT = "Nothing was pasted and the request is unchanged; the text is still on the clipboard.";

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

  /**
   * Attach files dropped from the operating system (their paths as the
   * desktop shell's drop event gave them; the backend reads only paths it
   * recorded from that drop itself). Same checks as picked files; content
   * already in the draft is not attached twice. Starts nothing. When
   * `current` says the conversation is gone by the time the files are read
   * (its project was closed, its task deleted), the results are discarded.
   */
  async dropFiles(key: string, paths: string[], current: () => boolean = () => true): Promise<void> {
    if (!paths.length) return;
    const taken = paths.slice(0, MAX_DROP_FILES);
    const extra = paths.length - taken.length;
    if (extra > 0) {
      reject(key, `${extra} more ${extra === 1 ? "file" : "files"}`, `At most ${MAX_DROP_FILES} files can be dropped at once.`);
    }
    await ingesting(key, async () => {
      let outcomes: IngestOutcome[];
      try {
        outcomes = await commands.agentsAttachmentsAddDropped(taken);
      } catch (e) {
        if (current()) reject(key, "Dropped files", errorMessage(e));
        return;
      }
      if (!current()) {
        discard(outcomes.flatMap((o) => (o.kind === "added" ? [o.attachment.id] : [])));
        return;
      }
      apply(key, outcomes, { distinct: true });
    });
  },

  /**
   * Attach a large pasted text as "pasted-text-N.txt" (the exact text, as
   * UTF-8) instead of inserting it. `at` is the field's value and selection
   * when it was pasted: the selected text is removed only after the
   * attachment exists and only if the text did not change meanwhile. On any
   * failure the draft stays as it was and the reason is shown. Sends nothing.
   */
  async pasteText(key: string, text: string, at: { value: string; start: number; end: number }): Promise<AttachmentInfo | null> {
    if (get(key).attachments.length >= MAX_ATTACHMENTS_PER_MESSAGE) {
      reject(key, "Pasted text", `${TOO_MANY} Remove one, then paste again. ${PASTE_KEPT}`);
      return null;
    }
    const n = (get(key).pastedTexts ?? 0) + 1;
    update(key, () => ({ pastedTexts: n }));
    const name = `pasted-text-${n}.txt`;
    const failed = (message: string) => {
      // The name is free again unless a later paste took the next one.
      update(key, (d) => (d.pastedTexts === n ? { pastedTexts: n - 1 } : {}));
      reject(key, name, `${message} ${PASTE_KEPT}`);
      return null;
    };
    return ingesting(key, async () => {
      let outcome: IngestOutcome;
      try {
        outcome = await commands.agentsAttachmentsAddBytes(name, "text/plain", utf8Base64(text));
      } catch (e) {
        return failed(errorMessage(e));
      }
      if (outcome.kind === "rejected") return failed(outcome.message);
      if (get(key).attachments.length >= MAX_ATTACHMENTS_PER_MESSAGE) {
        discard([outcome.attachment.id]);
        return failed(`${TOO_MANY} Remove one, then paste again.`);
      }
      const attachment = outcome.attachment;
      update(key, (d) => {
        // Text typed while the paste was read stays; then nothing is replaced.
        const unchanged = d.text === at.value;
        const paste: PastedText = {
          id: attachment.id,
          text,
          before: unchanged ? at.value.slice(0, at.start) : d.text,
          replaced: unchanged ? at.value.slice(at.start, at.end) : "",
        };
        return {
          text: unchanged ? at.value.slice(0, at.start) + at.value.slice(at.end) : d.text,
          attachments: [...d.attachments, attachment],
          pastes: [...(d.pastes ?? []), paste],
        };
      });
      return attachment;
    });
  },

  /** Whether a pasted attachment's text fits in the request when inserted. */
  pasteFitsInline(key: string, id: AttachmentId): boolean {
    const paste = (get(key).pastes ?? []).find((p) => p.id === id);
    return paste !== undefined && characters(get(key).text) + characters(paste.text) <= MAX_REQUEST_CHARS;
  },

  /**
   * Insert a pasted attachment's text into the request where it was pasted
   * (what a normal paste does) and remove the attachment. Refused, with the
   * attachment kept, when the request would exceed its length limit.
   */
  insertPasteInline(key: string, id: AttachmentId): boolean {
    const draft = get(key);
    const paste = (draft.pastes ?? []).find((p) => p.id === id);
    if (!paste) return false;
    if (!drafts.pasteFitsInline(key, id)) {
      const name = draft.attachments.find((a) => a.id === id)?.name ?? "Pasted text";
      reject(key, name, `The request can be at most ${MAX_REQUEST_CHARS.toLocaleString("en-US")} characters, so this text stays attached.`);
      return false;
    }
    unpaste(key, paste, insertAt(draft.text, pasteOffset(draft.text, paste), paste.text));
    return true;
  },

  /** Undo a paste's conversion: remove the attachment and put back the text it replaced. */
  undoPaste(key: string, id: AttachmentId) {
    const draft = get(key);
    const paste = (draft.pastes ?? []).find((p) => p.id === id);
    if (!paste) return;
    unpaste(key, paste, paste.replaced ? insertAt(draft.text, pasteOffset(draft.text, paste), paste.replaced) : draft.text);
  },

  /** Remove a draft attachment (the backend forgets it unless a message uses it). */
  remove(key: string, id: AttachmentId) {
    update(key, (d) => ({
      attachments: d.attachments.filter((a) => a.id !== id),
      pastes: (d.pastes ?? []).filter((p) => p.id !== id),
    }));
    discard([id]);
  },

  dismissRejections(key: string) {
    update(key, () => ({ rejections: [] }));
  },

  /**
   * "Chat about it": the next message from this draft discusses `request`.
   * Submits nothing. The draft's text and attachments stay exactly as they
   * are; the text is also remembered as `previousText` (kept from the first
   * Chat about it when one is already set) so an unrelated draft survives
   * the discussion.
   */
  chatAbout(key: string, request: { id: string; questions: readonly { prompt: string }[] }) {
    update(key, (d) => ({
      about: {
        requestId: request.id,
        prompt: request.questions[0]?.prompt ?? "",
        previousText: d.about ? d.about.previousText : d.text,
        seq: ++aboutSeq,
      },
    }));
  },

  /**
   * Stop discussing (the About chip's remove). Leaves text and attachments;
   * an empty field gets the unrelated draft text back.
   */
  clearAbout(key: string) {
    update(key, (d) => ({
      about: null,
      text: d.text === "" && d.about?.previousText ? d.about.previousText : d.text,
    }));
  },

  /**
   * After a discussion message was sent: remove the sent text (as
   * `completeSend` does) and the About; an empty field gets the unrelated
   * draft text back unless that text was part of what was sent.
   */
  completeDiscussion(key: string, sent: string) {
    update(key, (d) => {
      const previous = d.about?.previousText ?? "";
      const left = remainingText(d.text, sent);
      const restore = left === "" && previous.trim() !== "" && !sent.includes(previous.trim());
      return { about: null, text: restore ? previous : left };
    });
  },

  /**
   * After a successful send, clear only what was sent: text typed and
   * attachments added while the request ran stay in the draft.
   */
  completeSend(
    key: string,
    sent: { text: string; attachments: AttachmentId[]; model: ProviderChoice | null; rejections?: number[] },
  ) {
    update(key, (d) => ({
      text: remainingText(d.text, sent.text),
      attachments: d.attachments.filter((a) => !sent.attachments.includes(a.id)),
      pastes: (d.pastes ?? []).filter((p) => !sent.attachments.includes(p.id)),
      // Only the rejections shown when the message was sent are cleared.
      rejections: d.rejections.filter((r) => !(sent.rejections ?? []).includes(r.id)),
      model: d.model === sent.model ? null : d.model,
    }));
  },

  /** Tests only. */
  resetForTests() {
    useDrafts.setState({ drafts: {} });
  },
};

/**
 * The draft text left after `sent` was sent: empty when unchanged; the text
 * typed after it when the sent text is followed by a line or word break (so
 * a word is never cut); otherwise the whole current text, kept as is.
 */
export function remainingText(current: string, sent: string): string {
  if (current === sent) return "";
  if (sent && current.startsWith(sent) && /^\s/.test(current.slice(sent.length))) {
    return current.slice(sent.length).replace(/^\s+/, "");
  }
  return current;
}
