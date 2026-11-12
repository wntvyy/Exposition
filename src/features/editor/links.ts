// Links in editors (e.g. a URL in a Lean docstring shown in a hover). Opening
// one must never navigate the webview or launch another program, and the app
// has no opener permission: the link is copied to the clipboard instead, with a
// notification saying so. Monaco hands every link here (see `lib/monaco.ts`).
import { notify } from "../../app/notifications";

const MAX_SHOWN = 200;

/** Copy `uri` and tell the user; never opens it. */
export async function offerEditorLink(
  uri: string,
  clipboard: Pick<Clipboard, "writeText"> | undefined = globalThis.navigator?.clipboard,
): Promise<void> {
  const shown = uri.length > MAX_SHOWN ? `${uri.slice(0, MAX_SHOWN)}…` : uri;
  try {
    if (!clipboard) throw new Error("no clipboard");
    await clipboard.writeText(uri);
    notify({ tone: "info", title: "Link copied", body: `${shown}\nExposition does not open links; paste it into your browser.` });
  } catch {
    notify({ tone: "info", title: "Link not opened", body: `Exposition does not open links: ${shown}` });
  }
}
