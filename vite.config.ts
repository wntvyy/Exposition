/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The environment of the build command (the project has no Node type package).
const buildEnv = (globalThis as { process?: { env: Record<string, string | undefined> } }).process?.env ?? {};

// Tauri expects a fixed dev-server port and serves the built `dist/` in app builds.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    host: "127.0.0.1",
    port: 1420,
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/**", "**/target/**", "**/crates/**", "**/.worktrees/**", "**/.claude/**"],
    },
  },
  build: {
    target: "es2022",
    // Everything in `dist` is embedded in the app executable. A release build
    // (`tauri build` sets TAURI_ENV_DEBUG=false for this command) has no
    // developer tools to read source maps, and they are three quarters of the
    // bundle's bytes; debug builds and plain `vite build` keep them.
    sourcemap: buildEnv.TAURI_ENV_DEBUG !== "false",
    chunkSizeWarningLimit: 8000,
  },
  test: {
    include: ["src/**/*.test.{ts,tsx}", "scripts/**/*.test.mjs"],
    environment: "jsdom",
  },
});
