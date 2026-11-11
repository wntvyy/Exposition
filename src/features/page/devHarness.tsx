// DEBUG BUILDS ONLY — REMOVE AT INTEGRATION.
//
// Until the workbench shell renders registered editors and the workspace
// service opens files, debug builds expose `window.__expositionDev.openPage`
// to open a .typ file by absolute path (`dev_open_document`, a debug-only
// command) and show the page editor in a full-window overlay.
import { invoke } from "@tauri-apps/api/core";
import { createRoot, type Root } from "react-dom/client";

import type { DocumentInfo } from "../../bindings/DocumentInfo";
import { commands } from "../../lib/ipc";
import { installTypesettingEvents } from "../typesetting/store";
import { ProblemsPanel } from "../typesetting/ProblemsPanel";
import { PageEditor } from "./PageEditor";

declare global {
  interface Window {
    __expositionDev?: { openPage: (absolutePath: string) => Promise<DocumentInfo> };
  }
}

let root: Root | null = null;

export function installDevHarness(): void {
  void commands
    .appInfo()
    .then((info) => {
      if (!info.debugBuild) return;
      window.__expositionDev = {
        async openPage(absolutePath: string) {
          const info = await invoke<DocumentInfo>("dev_open_document", { path: absolutePath });
          installTypesettingEvents();
          let host = document.getElementById("exposition-dev-page");
          if (!host) {
            host = document.createElement("div");
            host.id = "exposition-dev-page";
            host.style.cssText = "position:fixed;inset:0;display:flex;flex-direction:column;z-index:10;background:Canvas";
            document.body.appendChild(host);
            root = createRoot(host);
          }
          const path = info.path ?? "";
          root?.render(
            <>
              <div style={{ flex: 1, minHeight: 0 }}>
                <PageEditor
                  key={path}
                  resource={{ key: `file:${path}`, path, title: path }}
                  mode="page"
                  active
                />
              </div>
              <div style={{ height: 140, borderTop: "1px solid #ccc", overflow: "auto" }}>
                <ProblemsPanel />
              </div>
            </>,
          );
          return info;
        },
      };
    })
    .catch(() => {});
}
