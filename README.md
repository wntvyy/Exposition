# Exposition

An agentic desktop workspace for turning ideas, research, computations,
references, and formal reasoning into mathematical and technical documents.

Exposition is a Tauri 2 application: a Rust core (`src-tauri/`, `crates/`) and a
React + TypeScript workbench (`src/`). Source editing uses Monaco; the page
editor edits the compiler's actual page layout. Both go through one revisioned
document service.

## Status

Early development. Windows is the primary verified platform; macOS and Linux
are structurally supported but unverified.

## Prerequisites

- Node.js 22 or newer and npm
- Rust (stable, 1.92+) with the platform's Tauri prerequisites
  (<https://v2.tauri.app/start/prerequisites/>)
- Optional toolchains, discovered at runtime: a TeX distribution (pdfLaTeX,
  LuaLaTeX, latexmk), Python with `jupyter_client`/`ipykernel`, R/IRkernel,
  Julia/IJulia, and Lean (via elan). Missing toolchains are reported in the app.

## Development

```sh
npm ci                 # install frontend dependencies
npm run dev            # run the desktop app with hot reload
npm run check          # all checks: private paths, typecheck, tests, build, fmt, clippy, bindings
npm run build:app      # debug desktop build with the bundled frontend (target/debug)
npm run e2e            # end-to-end scenarios against the built desktop app
npm run build:release  # release build and installer
```

`npm run check -- --skip-rust` runs only the frontend checks.

### Layout

| Path | Contents |
|---|---|
| `crates/exposition-core` | Shared contracts: text positions, document transactions, jobs, builds, artifacts |
| `crates/exposition-*` | Domain services (workspace/documents, execution, typesetting, ...) |
| `src-tauri` | Application wiring: state, commands, events, capabilities |
| `src/bindings` | TypeScript types generated from the Rust contracts (`cargo test`); do not edit |
| `src/app` | Workbench composition contract |
| `src/ui` | Design tokens and shared components |
| `src/features/<domain>` | Feature views |
| `scripts/e2e` | End-to-end harness driving the real desktop app over CDP |

Contract types are generated: change the Rust type, run `cargo test`, and commit
the regenerated `src/bindings`. `npm run check:bindings` fails on drift.
