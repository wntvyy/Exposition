// Workspace trust prompt. `requestWorkspaceTrust(root)` is the only place the
// frontend grants trust, and only after the user explicitly confirms in the
// shared alert dialog. Other features call it when an action needs trusted
// native execution (e.g. after a job was refused with `untrustedWorkspace`).
import { dismiss, notify } from "../../app/notifications";
import { commands, errorMessage } from "../../lib/ipc";
import { confirm } from "../../ui/components";
import { setTrustPrompt } from "./store";
import "./jobs.css";

let open: Promise<boolean> | null = null;

/**
 * Ask the user whether to trust a workspace folder. Resolves true if it is (or
 * already was) trusted, false if the user declined or granting failed.
 */
export function requestWorkspaceTrust(root: string): Promise<boolean> {
  if (open) return open;
  open = (async () => {
    try {
      let canonical = root;
      try {
        const status = await commands.trustStatus(root);
        if (status.trusted) return true;
        canonical = status.root;
      } catch {
        // Fall through: the dialog still shows the requested path and granting reports the error.
      }
      const choice = await confirm<"cancel" | "trust">({
        title: "Trust this workspace?",
        message: (
          <div className="exp-trust" data-testid="trust-dialog">
            <code className="exp-trust__path" data-testid="trust-root">
              {canonical}
            </code>
            <p>
              Builds, kernels, and commands in a trusted workspace run on this computer with your account&rsquo;s
              permissions and can read or change your files. Trust only folders whose contents you trust.
            </p>
            <p className="exp-muted">
              Without trust you can still open and edit files. Restricted (container) execution stays available when it
              is configured.
            </p>
          </div>
        ),
        actions: [
          { id: "cancel", label: "Don’t trust", variant: "secondary" },
          { id: "trust", label: "Trust workspace", variant: "primary" },
        ],
        cancelId: "cancel",
        // The safe choice is focused; trusting is always a deliberate action.
        defaultId: "cancel",
        tone: "warning",
      });
      if (choice !== "trust") return false;
      try {
        const status = await commands.trustGrant(canonical);
        if (status.trusted) {
          // Retire any "workspace not trusted" prompt for this folder.
          dismiss(`trust-${root}`);
          dismiss(`trust-${status.root}`);
          notify({ tone: "success", title: "Workspace trusted", body: status.root });
          return true;
        }
      } catch (e) {
        notify({ tone: "danger", title: "Could not trust workspace", body: errorMessage(e) });
      }
      return false;
    } finally {
      open = null;
    }
  })();
  return open;
}

setTrustPrompt(requestWorkspaceTrust);
