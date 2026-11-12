// Feature registration entry point. Each feature adds exactly one side-effect
// import of its `register.ts`, which contributes views, editors, and commands to
// the workbench registry (`src/app/registry.ts`). Keep this list sorted.
import "./agents/register";
import "./editor/register";
import "./jobs/register";
import "./lean/register";
import "./research/register";
import "./page/register";
import "./typesetting/register";
import "./workspace/register";

export {};
