/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri expects a fixed dev-server port and serves the built `dist/` in app builds.
export default defineConfig(({ mode }) => ({
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
    // Everything in `dist` is embedded in the app executable. The release
    // build (`npm run build:release` builds the frontend with `--mode release`)
    // has no developer tools to read source maps, and they are three quarters
    // of the bundle's bytes; debug builds and plain `vite build` keep them.
    sourcemap: mode !== "release",
    chunkSizeWarningLimit: 8000,
  },
  test: {
    include: ["src/**/*.test.{ts,tsx}", "scripts/**/*.test.mjs"],
    environment: "jsdom",
  },
}));
