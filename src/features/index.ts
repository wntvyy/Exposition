// Feature registration entry point. Each feature adds exactly one side-effect
// import of its `register.ts`, which contributes views, editors, and commands to
// the workbench registry (`src/app/registry.ts`). Keep this list sorted.
import "./agents/register";
import "./editor/register";
import "./jobs/register";
import "./lean/register";
import "./research/register";
import "./settings/register";
import "./page/register";
import "./projects/register";
import "./typesetting/register";
import "./workflows/register";
import "./workspace/register";

export {};
