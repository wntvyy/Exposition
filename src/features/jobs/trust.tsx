// Workspace trust prompt. `requestWorkspaceTrust(root)` is the only place the
// frontend grants trust, and only after the user explicitly confirms. Other
// features call it when an action needs trusted native execution.
import { useState } from "react";
import { createRoot } from "react-dom/client";
import { Button, Dialog, Heading, Modal, ModalOverlay } from "react-aria-components";
import { ShieldAlert } from "lucide-react";

import { commands, errorMessage } from "../../lib/ipc";
import { setTrustPrompt } from "./store";
import "./jobs.css";

let open: Promise<boolean> | null = null;

/**
 * Ask the user whether to trust a workspace folder. Resolves true if it is (or
 * already was) trusted, false if the user declined or the request failed.
 */
export function requestWorkspaceTrust(root: string): Promise<boolean> {
  if (open) return open;
  open = (async () => {
    try {
      const status = await commands.trustStatus(root);
      if (status.trusted) return true;
      return await showDialog(status.root);
    } catch {
      return await showDialog(root);
    } finally {
      open = null;
    }
  })();
  return open;
}

function showDialog(root: string): Promise<boolean> {
  return new Promise((resolve) => {
    const host = document.createElement("div");
    host.dataset.testid = "trust-dialog-host";
    document.body.appendChild(host);
    const reactRoot = createRoot(host);
    const close = (trusted: boolean) => {
      reactRoot.unmount();
      host.remove();
      resolve(trusted);
    };
    reactRoot.render(<TrustDialog root={root} onClose={close} />);
  });
}

function TrustDialog({ root, onClose }: { root: string; onClose: (trusted: boolean) => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const trust = async () => {
    setBusy(true);
    setError(null);
    try {
      const status = await commands.trustGrant(root);
      if (status.trusted) onClose(true);
      else setError("The workspace is still not trusted.");
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <ModalOverlay
      className="exp-trust-overlay"
      isOpen
      isDismissable
      onOpenChange={(isOpen) => {
        if (!isOpen) onClose(false);
      }}
    >
      <Modal className="exp-trust-modal">
        <Dialog className="exp-trust-dialog" data-testid="trust-dialog" role="alertdialog">
          <Heading slot="title" className="exp-trust-title">
            <ShieldAlert size={18} aria-hidden /> Trust this workspace?
          </Heading>
          <p className="exp-trust-path" data-testid="trust-root">
            {root}
          </p>
          <p>
            Builds, kernels, and commands in a trusted workspace run on this computer with your account&rsquo;s
            permissions and can read or change your files. Trust only folders whose contents you trust.
          </p>
          <p className="exp-muted">
            Without trust you can still open and edit files. Restricted (container) execution stays available when it is
            configured.
          </p>
          {error && (
            <p role="alert" className="exp-error">
              {error}
            </p>
          )}
          <div className="exp-trust-actions">
            <Button className="exp-button" autoFocus onPress={() => onClose(false)} isDisabled={busy}>
              Don&rsquo;t trust
            </Button>
            <Button className="exp-button exp-button-primary" onPress={() => void trust()} isDisabled={busy}>
              Trust workspace
            </Button>
          </div>
        </Dialog>
      </Modal>
    </ModalOverlay>
  );
}

setTrustPrompt(requestWorkspaceTrust);
