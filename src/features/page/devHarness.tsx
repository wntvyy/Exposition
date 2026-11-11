// DEBUG BUILDS ONLY — REMOVE AT INTEGRATION.
//
// Until the workspace service opens projects and files, debug builds expose
// `window.__expositionDev.openPage(absolutePath)`: it opens a .typ file with
// the debug-only `dev_open_document` command and shows it in the workbench as
// a Page editor (through the normal editor registry).
import { invoke } from "@tauri-apps/api/core";

import { editors } from "../../app/editors";
import { layout } from "../../app/layout";
import type { DocumentInfo } from "../../bindings/DocumentInfo";
import { commands } from "../../lib/ipc";

declare global {
  interface Window {
    __expositionDev?: {
      openPage: (absolutePath: string) => Promise<DocumentInfo>;
      showProblems: () => void;
    };
  }
}

export function installDevHarness(): void {
  void commands
    .appInfo()
    .then((info) => {
      if (!info.debugBuild) return;
      window.__expositionDev = {
        async openPage(absolutePath: string) {
          const doc = await invoke<DocumentInfo>("dev_open_document", { path: absolutePath });
          const path = doc.path ?? "";
          editors.open({ key: `file:${path}`, path, title: path.split("/").pop() ?? path }, { mode: "page" });
          return doc;
        },
        showProblems() {
          layout.showView("panel", "typesetting.problems");
        },
      };
    })
    .catch(() => {});
}
