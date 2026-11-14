// Project operations (P1): the focus-scoped Edit commands (Copy, Paste,
// Duplicate, Delete, Delete Permanently, Undo, Redo) and focus tracking.
// Surfaces register themselves (`useOpSurface`); the decision dialog uses the
// shell's alert host and results use the shell's notifications.
import { registry } from "../../app/registry";
import { operationCommands, startSurfaceTracking } from "./surfaces";
import "./operations.css";

startSurfaceTracking();
for (const command of operationCommands()) registry.registerCommand(command);
