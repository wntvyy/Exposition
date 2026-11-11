// Modal choice dialog (Save / Don't Save / Cancel, overwrite confirmations).
// Mounted in its own root so it works regardless of which workbench shell is
// rendering, including while a close guard is awaiting an answer.
import { useEffect, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { Button, Dialog, Heading, Modal, ModalOverlay } from "react-aria-components";

import "./workspace.css";

export interface PromptButton {
  id: string;
  label: string;
  /** The default action (rendered as primary, receives initial focus). */
  primary?: boolean;
  /** Chosen when the dialog is dismissed with Escape. */
  cancel?: boolean;
  danger?: boolean;
}

export interface PromptOptions {
  title: string;
  message: string;
  buttons: PromptButton[];
}

interface Pending extends PromptOptions {
  resolve: (id: string) => void;
}

let root: Root | null = null;
const queue: Pending[] = [];
let notify: (() => void) | null = null;

function PromptHost() {
  const [, setVersion] = useState(0);
  useEffect(() => {
    notify = () => setVersion((v) => v + 1);
    return () => {
      notify = null;
    };
  }, []);
  const current = queue[0];
  if (!current) return null;
  const finish = (id: string) => {
    queue.shift();
    current.resolve(id);
    notify?.();
  };
  const cancel = current.buttons.find((b) => b.cancel) ?? current.buttons[current.buttons.length - 1];
  return (
    <ModalOverlay
      isOpen
      isDismissable={false}
      isKeyboardDismissDisabled={false}
      onOpenChange={(open) => {
        if (!open && cancel) finish(cancel.id);
      }}
      className="xp-prompt-overlay"
    >
      <Modal className="xp-prompt-modal">
        <Dialog role="alertdialog" className="xp-prompt" data-testid="prompt">
          <Heading slot="title" className="xp-prompt-title">
            {current.title}
          </Heading>
          <p className="xp-prompt-message">{current.message}</p>
          <div className="xp-prompt-actions">
            {current.buttons.map((button) => (
              <Button
                key={button.id}
                autoFocus={button.primary}
                className={`xp-button${button.primary ? " xp-button-primary" : ""}${button.danger ? " xp-button-danger" : ""}`}
                data-prompt-button={button.id}
                onPress={() => finish(button.id)}
              >
                {button.label}
              </Button>
            ))}
          </div>
        </Dialog>
      </Modal>
    </ModalOverlay>
  );
}

/** Ask the user to choose one of `buttons`; resolves with the chosen button id. */
export function ask(options: PromptOptions): Promise<string> {
  if (!root) {
    const container = document.createElement("div");
    container.id = "xp-prompt-root";
    document.body.appendChild(container);
    root = createRoot(container);
    root.render(<PromptHost />);
  }
  return new Promise((resolve) => {
    queue.push({ ...options, resolve });
    notify?.();
  });
}
