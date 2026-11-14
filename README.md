# Exposition

An agentic desktop workspace for turning ideas, research, computations,
references, and formal reasoning into mathematical and technical documents.

Exposition is a Tauri 2 application: a Rust core (`src-tauri/`, `crates/`) and a
React + TypeScript workbench (`src/`). Source editing uses Monaco; the page
editor edits the compiler's actual page layout. Both go through one revisioned
document service.

## Status

Local alpha. Windows is the primary verified platform; macOS and Linux are
structurally supported but unverified. See [docs/capabilities.md](docs/capabilities.md)
for exactly what works per format and subsystem, and its known limitations.
[docs/functionality.md](docs/functionality.md) is the complete reference: every
view, command, IPC endpoint, event, setting, external connection, and stored file.

## Prerequisites

- Node.js 22 or newer and npm
- Rust through rustup with the platform's Tauri prerequisites
  (<https://v2.tauri.app/start/prerequisites/>). `rust-toolchain.toml` pins the
  toolchain that local checks and CI run on (1.96.0); rustup installs it on
  first use. The crates build on Rust 1.92 or newer.
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
| `crates/exposition-core` | Shared contracts: text positions, document transactions, jobs, builds, artifacts, project operations (plan → apply → restore) |
| `crates/exposition-workspace` | Project files and documents: buffers, safe saves, recovery, external changes, and the file engine of project operations (Recycle Bin, collision-free copies, restore) |
| `crates/exposition-graph` | Project graph: typed vocabulary, validated SQLite store with backups and recovery, reconciliation with files and open buffers, impact, operation journal, project blueprints, workflow definitions (schemas /1 and /2, composites, validation) and bundles |
| `crates/exposition-workflow` | Workflow runs: reviewed plans and approval hashes, the bounded parallel scheduler, step adapters, conditions and repeats, triggers, environment capture, run manifests, reproduction; the data side of project operations and Storage & History |
| `crates/exposition-*` | Other domain services (execution, typesetting, research, Lean, agents, ...) |
| `src-tauri` | Application wiring: state, commands, events, capabilities |
| `src/bindings` | TypeScript types generated from the Rust contracts (`cargo test`); do not edit |
| `src/app` | Workbench composition contract |
| `src/ui` | Design tokens and shared components |
| `src/features/<domain>` | Feature views |
| `scripts/e2e` | End-to-end harness driving the real desktop app over CDP |

Contract types are generated: change the Rust type, run `cargo test`, and commit
the regenerated `src/bindings`. `npm run check:bindings` fails on drift.
