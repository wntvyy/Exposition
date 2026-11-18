// V2 UI samples harness (not production code). Run from the checkout root:
//   npx vite --config prototypes/v2-samples/vite.config.ts
import { relative } from "node:path";
import { fileURLToPath } from "node:url";

import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

const root = fileURLToPath(new URL(".", import.meta.url));
const repo = fileURLToPath(new URL("../..", import.meta.url));

export default defineConfig({
  root,
  plugins: [react()],
  clearScreen: false,
  server: {
    host: "127.0.0.1",
    port: 5175,
    strictPort: true,
    fs: { allow: [repo] },
    // Relative to the repo only: this checkout itself may live under `.worktrees/`.
    watch: {
      ignored: (path: string) => {
        const rel = relative(repo, path).replaceAll("\\", "/");
        return /^(src-tauri|target|crates|node_modules|\.git|\.worktrees)(\/|$)/.test(rel) || rel.startsWith("prototypes/v2-samples/shots");
      },
    },
  },
  optimizeDeps: {
    entries: ["main.tsx"],
  },
});
