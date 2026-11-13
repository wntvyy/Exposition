// Visual typed authoring of a workflow draft (React Flow, base.css only,
// themed with tokens; conventions of the Graph canvas, UI_STANDARD
// "Canvas"/"Workflow runs").
//
// - Input nodes, step nodes with one labelled typed handle per input/output
//   port (shape + color by port type, the type in text; required inputs
//   marked "required"), outcome nodes.
// - Data connections are solid; prerequisites dashed ("runs before (no
//   data)"); annotations dotted ("annotation (no data)").
// - Dragging from an output onto an input adds a data binding only when
//   `connect` (the form's compatibility rule) accepts it; otherwise the
//   reason is announced and nothing changes. Step bottom → step top adds a
//   prerequisite. Select + Delete removes a connection or a step.
// - Layout is arranged by dependency order; moved positions are kept per
//   workflow in localStorage (presentation only, never in the definition).
// - Validation diagnostics appear as badges on the nodes and ports they name.
// The keyboard alternative is the form (the inspector below the canvas).
import "@xyflow/react/dist/base.css";
import "../graph/canvas/canvas.css";

import {
  applyNodeChanges,
  Handle,
  MarkerType,
  Panel,
  Position,
  ReactFlow,
  ReactFlowProvider,
  useReactFlow,
  useViewport,
  type Connection,
  type Edge,
  type Node,
  type NodeChange,
  type NodeProps,
} from "@xyflow/react";
import { Focus, LayoutGrid, Maximize, Minus, Plus } from "lucide-react";
import { createContext, memo, useCallback, useContext, useEffect, useMemo, useState, type KeyboardEvent } from "react";

import type { OperationContract } from "../../bindings/OperationContract";
import type { PortSpec } from "../../bindings/PortSpec";
import type { PortType } from "../../bindings/PortType";
import type { WorkflowDefinition } from "../../bindings/WorkflowDefinition";
import type { WorkflowDiagnostic } from "../../bindings/WorkflowDiagnostic";
import { Badge, Banner, IconButton, Toolbar } from "../../ui/components";
import {
  autoLayout,
  connect,
  edgesOf,
  loadPositions,
  nodeIdOf,
  parseNodeId,
  PRE_IN,
  PRE_OUT,
  removeEdge,
  savePositions,
  type CanvasSelectionItem,
  type Point,
} from "./canvasModel";
import { contractOf, OUTCOME_LABELS, PORT_TYPE_LABELS, removeStep } from "./model";

export type { CanvasSelectionItem } from "./canvasModel";

/** Port type → token category (color) and handle shape (a second, non-color cue; the type is also text). */
export const PORT_VISUAL: Record<PortType, { category: string; shape: "circle" | "square" | "diamond" }> = {
  codeSource: { category: "document", shape: "square" },
  document: { category: "document", shape: "square" },
  formalSource: { category: "formal", shape: "square" },
  dataset: { category: "data", shape: "circle" },
  table: { category: "data", shape: "circle" },
  figure: { category: "data", shape: "circle" },
  retainedArtifact: { category: "data", shape: "circle" },
  formalEvidence: { category: "formal", shape: "diamond" },
  reviewReport: { category: "reference", shape: "diamond" },
  promotionProposal: { category: "math", shape: "diamond" },
  associationProposal: { category: "math", shape: "diamond" },
};

interface NodeContextValue {
  draft: WorkflowDefinition;
  contracts: OperationContract[];
  diagnostics: WorkflowDiagnostic[];
}
const NodeContext = createContext<NodeContextValue | null>(null);
const useNodeContext = () => useContext(NodeContext) as NodeContextValue;

type WfNodeData = { ref: string };
type WfNode = Node<WfNodeData, "wfStep" | "wfInput" | "wfOutcome">;

function ProblemBadge({ items }: { items: WorkflowDiagnostic[] }) {
  const errors = items.filter((d) => d.severity === "error");
  if (items.length === 0) return null;
  const text = items.map((d) => d.message).join("\n");
  return (
    <Badge tone={errors.length ? "danger" : "warning"} title={text}>
      {errors.length ? `${errors.length} problem${errors.length === 1 ? "" : "s"}` : `${items.length} note${items.length === 1 ? "" : "s"}`}
    </Badge>
  );
}

function PortRow({ side, spec, stepId }: { side: "in" | "out"; spec: PortSpec; stepId: string }) {
  const { diagnostics } = useNodeContext();
  const problems = diagnostics.filter((d) => d.step === stepId && d.port === spec.name && d.outcome === null);
  const type = spec.types[0] ?? "dataset";
  const visual = PORT_VISUAL[type];
  const types = spec.types.map((t) => PORT_TYPE_LABELS[t]).join(" or ");
  const name = `${side === "in" ? "Input" : "Output"} ${spec.label} (${types})${side === "in" && spec.required ? ", required" : ""}`;
  return (
    <li className="xp-wfc-port" data-side={side} data-port={spec.name} data-testid={`wfc-port-${stepId}-${side}-${spec.name}`}>
      <Handle
        type={side === "in" ? "target" : "source"}
        position={side === "in" ? Position.Left : Position.Right}
        id={`${side}:${spec.name}`}
        className="xp-wfc-handle"
        data-shape={visual.shape}
        data-category={visual.category}
        aria-label={name}
        title={name}
      />
      <span className="xp-wfc-port__label">
        {spec.label}
        <span className="xp-wfc-port__type"> · {types}</span>
        {side === "in" && spec.required && <span className="xp-wfc-port__required"> · required</span>}
      </span>
      <ProblemBadge items={problems} />
    </li>
  );
}

function StepNodeView({ data, selected }: NodeProps<WfNode>) {
  const { draft, contracts, diagnostics } = useNodeContext();
  const step = draft.steps.find((s) => s.id === data.ref);
  if (!step) return null;
  const contract = contractOf(contracts, step);
  const own = diagnostics.filter((d) => d.step === step.id && d.port === null && d.outcome === null);
  return (
    <div className="xp-wfc-node ui-node-card" data-category="workflow" data-selected={selected || undefined} data-step={step.id} data-testid={`wfc-step-${step.id}`}>
      <Handle type="target" position={Position.Top} id={PRE_IN} className="xp-wfc-handle xp-wfc-handle--pre" aria-label={`Runs after (prerequisite into ${step.label})`} title="Drop a prerequisite here: this step runs after that one (no data)" />
      <div className="ui-node-card__header">
        <span className="ui-node-card__type">
          <span>{contract ? contract.title : "Unknown operation"}</span>
        </span>
      </div>
      <p className="ui-node-card__title" title={step.label}>
        {step.label}
      </p>
      <div className="ui-node-card__badges">
        {!contract && (
          <Badge tone="danger" dashed>
            Not a registered operation
          </Badge>
        )}
        {contract && !contract.runner.available && (
          <Badge tone="neutral" dashed title={contract.runner.availability}>
            Runner unavailable
          </Badge>
        )}
        <ProblemBadge items={own} />
      </div>
      {contract && (
        <ul className="xp-wfc-ports" aria-label={`Ports of ${step.label}`}>
          {contract.inputs.map((p) => (
            <PortRow key={`in-${p.name}`} side="in" spec={p} stepId={step.id} />
          ))}
          {contract.outputs.map((p) => (
            <PortRow key={`out-${p.name}`} side="out" spec={p} stepId={step.id} />
          ))}
        </ul>
      )}
      <Handle type="source" position={Position.Bottom} id={PRE_OUT} className="xp-wfc-handle xp-wfc-handle--pre" aria-label={`Runs before (drag to a step that should run after ${step.label})`} title="Drag to another step's top edge: that step runs after this one (no data)" />
    </div>
  );
}

function InputNodeView({ data, selected }: NodeProps<WfNode>) {
  const { draft, diagnostics } = useNodeContext();
  const input = draft.inputs.find((i) => i.id === data.ref);
  if (!input) return null;
  const visual = PORT_VISUAL[input.portType];
  const path = input.source.kind === "resource" ? input.source.path : null;
  const name = `Output of input ${input.label} (${PORT_TYPE_LABELS[input.portType]})`;
  return (
    <div className="xp-wfc-node ui-node-card" data-category={visual.category} data-selected={selected || undefined} data-testid={`wfc-input-${input.id}`}>
      <div className="ui-node-card__header">
        <span className="ui-node-card__type">
          <span>Input · {PORT_TYPE_LABELS[input.portType]}</span>
        </span>
      </div>
      <p className="ui-node-card__title" title={input.label}>
        {input.label}
      </p>
      <div className="ui-node-card__badges">
        {path ? <code className="xp-wfc-path">{path}</code> : <Badge tone="warning" dashed>No file chosen</Badge>}
        <ProblemBadge items={diagnostics.filter((d) => d.input === input.id)} />
      </div>
      <Handle type="source" position={Position.Right} id="out" className="xp-wfc-handle" data-shape={visual.shape} data-category={visual.category} aria-label={name} title={name} />
    </div>
  );
}

function OutcomeNodeView({ data, selected }: NodeProps<WfNode>) {
  const { draft, diagnostics } = useNodeContext();
  const outcome = draft.outcomes.find((o) => o.id === data.ref);
  if (!outcome) return null;
  return (
    <div className="xp-wfc-node ui-node-card" data-category="workflow" data-inactive={outcome.optional || undefined} data-selected={selected || undefined} data-testid={`wfc-outcome-${outcome.id}`}>
      <Handle type="target" position={Position.Left} id="in" className="xp-wfc-handle" data-shape="diamond" data-category="workflow" aria-label={`Realizes the outcome ${outcome.label}`} title={`Realizes ${outcome.label}`} />
      <div className="ui-node-card__header">
        <span className="ui-node-card__type">
          <span>Outcome · {OUTCOME_LABELS[outcome.kind]}</span>
        </span>
      </div>
      <p className="ui-node-card__title" title={outcome.label}>
        {outcome.label}
      </p>
      <div className="ui-node-card__badges">
        {outcome.optional && <Badge tone="neutral" dashed>Optional</Badge>}
        <ProblemBadge items={diagnostics.filter((d) => d.outcome === outcome.id)} />
      </div>
    </div>
  );
}

const nodeTypes = { wfStep: memo(StepNodeView), wfInput: memo(InputNodeView), wfOutcome: memo(OutcomeNodeView) };

const reducedMotion = () => typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
const duration = () => (reducedMotion() ? 0 : 200);

function Controls({ onArrange }: { onArrange: () => void }) {
  const flow = useReactFlow();
  const { zoom } = useViewport();
  return (
    <Panel position="top-right" className="xp-graph-controls">
      <Toolbar aria-label="Canvas view controls" className="xp-graph-controls__toolbar">
        <IconButton icon={Minus} size="sm" aria-label="Zoom out" keybinding="-" onPress={() => void flow.zoomOut({ duration: duration() })} />
        <span className="xp-graph-controls__zoom" aria-live="off">
          {Math.round(zoom * 100)}%
        </span>
        <IconButton icon={Plus} size="sm" aria-label="Zoom in" keybinding="+" onPress={() => void flow.zoomIn({ duration: duration() })} />
        <IconButton icon={Maximize} size="sm" aria-label="Fit to view" keybinding="Shift+1" onPress={() => void flow.fitView({ padding: 0.15, duration: duration() })} />
        <IconButton icon={Focus} size="sm" aria-label="Reset view (100%)" keybinding="0" onPress={() => void flow.setViewport({ x: 0, y: 0, zoom: 1 }, { duration: duration() })} />
        <IconButton icon={LayoutGrid} size="sm" aria-label="Arrange by dependency order" onPress={onArrange} />
      </Toolbar>
    </Panel>
  );
}

export interface WorkflowCanvasProps {
  workflowId: string;
  draft: WorkflowDefinition;
  contracts: OperationContract[];
  diagnostics: WorkflowDiagnostic[];
  selection: CanvasSelectionItem | null;
  onSelectionChange: (selection: CanvasSelectionItem | null) => void;
  onChange: (draft: WorkflowDefinition) => void;
}

function toSelection(nodeId: string): CanvasSelectionItem | null {
  const parsed = parseNodeId(nodeId);
  return parsed ? { kind: parsed.kind, id: parsed.id } : null;
}

function selectedNodeId(selection: CanvasSelectionItem | null): string | null {
  if (!selection || selection.kind === "edge") return null;
  return nodeIdOf[selection.kind](selection.id);
}

function CanvasInner({ workflowId, draft, contracts, diagnostics, selection, onSelectionChange, onChange }: WorkflowCanvasProps) {
  const flow = useReactFlow();
  const [saved, setSaved] = useState<Record<string, Point>>(() => loadPositions(workflowId));
  const [message, setMessage] = useState<{ ok: boolean; text: string } | null>(null);
  useEffect(() => setSaved(loadPositions(workflowId)), [workflowId]);

  const layout = useMemo(() => autoLayout(draft, contracts), [draft, contracts]);
  const selectedId = selectedNodeId(selection);

  const desired = useMemo<WfNode[]>(() => {
    const make = (id: string, type: WfNode["type"], ref: string): WfNode => ({
      id,
      type,
      data: { ref },
      position: saved[id] ?? layout.get(id) ?? { x: 0, y: 0 },
      selected: id === selectedId,
      ariaLabel: undefined,
    });
    return [
      ...draft.inputs.map((i) => make(nodeIdOf.input(i.id), "wfInput", i.id)),
      ...draft.steps.map((s) => make(nodeIdOf.step(s.id), "wfStep", s.id)),
      ...draft.outcomes.map((o) => make(nodeIdOf.outcome(o.id), "wfOutcome", o.id)),
    ].map((n) => {
      const parsed = parseNodeId(n.id)!;
      const label =
        parsed.kind === "step"
          ? `Step ${draft.steps.find((s) => s.id === parsed.id)?.label ?? parsed.id}`
          : parsed.kind === "input"
            ? `Input ${draft.inputs.find((i) => i.id === parsed.id)?.label ?? parsed.id}`
            : `Outcome ${draft.outcomes.find((o) => o.id === parsed.id)?.label ?? parsed.id}`;
      return { ...n, ariaLabel: label };
    });
  }, [draft, layout, saved, selectedId]);

  const [nodes, setNodes] = useState<WfNode[]>(desired);
  useEffect(() => setNodes(desired), [desired]);

  const wfEdges = useMemo(() => edgesOf(draft, contracts, diagnostics), [draft, contracts, diagnostics]);
  const edges = useMemo<Edge[]>(
    () =>
      wfEdges.map((e) => ({
        id: e.id,
        source: e.source,
        sourceHandle: e.sourceHandle,
        target: e.target,
        targetHandle: e.targetHandle,
        label: e.problems.length ? `${e.label} — problem` : e.label,
        ariaLabel: e.problems.length ? `${e.description}. Problem: ${e.problems.join("; ")}` : e.description,
        className: `xp-wfc-edge xp-wfc-edge--${e.kind}${e.problems.length ? " xp-wfc-edge--problem" : ""}`,
        selected: selection?.kind === "edge" && selection.id === e.id,
        markerEnd: { type: e.kind === "data" || e.kind === "outcome" ? MarkerType.ArrowClosed : MarkerType.Arrow, color: "var(--color-graph-edge)" },
        data: { kind: e.kind },
        deletable: e.kind !== "outcome",
      })),
    [wfEdges, selection],
  );

  const onNodesChange = useCallback(
    (changes: NodeChange<WfNode>[]) => {
      setNodes((current) => applyNodeChanges(changes, current));
      const moved: Record<string, Point> = {};
      for (const change of changes) {
        if (change.type === "position" && change.dragging === false && change.position) moved[change.id] = { ...change.position };
        if (change.type === "select" && change.selected) onSelectionChange(toSelection(change.id));
      }
      if (Object.keys(moved).length) {
        setSaved((prev) => {
          const next = { ...prev, ...moved };
          savePositions(workflowId, next);
          return next;
        });
      }
    },
    [onSelectionChange, workflowId],
  );

  const onConnect = useCallback(
    (c: Connection) => {
      const result = connect(draft, contracts, { source: c.source, sourceHandle: c.sourceHandle ?? null, target: c.target, targetHandle: c.targetHandle ?? null });
      setMessage({ ok: result.ok, text: result.message });
      if (result.ok) onChange(result.draft);
    },
    [draft, contracts, onChange],
  );

  const removeSelected = () => {
    if (!selection) return false;
    if (selection.kind === "edge") {
      const next = removeEdge(draft, selection.id);
      if (!next) {
        setMessage({ ok: false, text: "An outcome link is changed by connecting another output to the outcome, or in the Form view." });
        return true;
      }
      onChange(next);
      onSelectionChange(null);
      setMessage({ ok: true, text: "Connection removed." });
      return true;
    }
    if (selection.kind === "step") {
      const label = draft.steps.find((s) => s.id === selection.id)?.label ?? selection.id;
      onChange(removeStep(draft, selection.id));
      onSelectionChange(null);
      setMessage({ ok: true, text: `Step “${label}” removed with its connections. Save to keep the change.` });
      return true;
    }
    return false;
  };

  const arrange = () => {
    setSaved({});
    savePositions(workflowId, {});
    requestAnimationFrame(() => void flow.fitView({ padding: 0.15, maxZoom: 1, duration: duration() }));
  };

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const target = event.target as HTMLElement;
    if (target.closest("input, textarea, [contenteditable]") || event.ctrlKey || event.metaKey || event.altKey) return;
    const d = duration();
    if (event.key === "Delete" || event.key === "Backspace") {
      if (!removeSelected()) return;
    } else if (event.key === "+" || event.key === "=") void flow.zoomIn({ duration: d });
    else if (event.key === "-" || event.key === "_") void flow.zoomOut({ duration: d });
    else if (event.shiftKey && event.code === "Digit1") void flow.fitView({ padding: 0.15, duration: d });
    else if (event.key === "0" && !event.shiftKey) void flow.setViewport({ x: 0, y: 0, zoom: 1 }, { duration: d });
    else if (event.key === "Escape") onSelectionChange(null);
    else return;
    event.preventDefault();
  };

  const context = useMemo(() => ({ draft, contracts, diagnostics }), [draft, contracts, diagnostics]);

  return (
    <NodeContext.Provider value={context}>
      <div className="xp-wfc" data-testid="workflow-canvas">
        <div className="xp-wfc__message" role="status" aria-live="polite" data-testid="workflow-canvas-message" data-outcome={message ? (message.ok ? "accepted" : "refused") : undefined}>
          {message && (
            <Banner tone={message.ok ? "info" : "warning"} title={message.ok ? "Changed" : "Not connected"}>
              {message.text}
            </Banner>
          )}
        </div>
        <div
          className="xp-graph-canvas xp-wfc__canvas"
          onKeyDown={onKeyDown}
          role="group"
          aria-roledescription="canvas"
          aria-label={`Workflow canvas of ${draft.title || "this workflow"}. Drag from an output to a compatible input to connect; Delete removes the selected connection or step.`}
        >
          <ReactFlow<WfNode, Edge>
            nodes={nodes}
            edges={edges}
            nodeTypes={nodeTypes}
            onNodesChange={onNodesChange}
            onEdgesChange={(changes) => {
              for (const change of changes) if (change.type === "select" && change.selected) onSelectionChange({ kind: "edge", id: change.id });
            }}
            onConnect={onConnect}
            onPaneClick={() => onSelectionChange(null)}
            fitView
            fitViewOptions={{ padding: 0.15, maxZoom: 1 }}
            nodesConnectable
            nodesDraggable
            nodesFocusable
            edgesFocusable
            elementsSelectable
            edgesReconnectable={false}
            deleteKeyCode={null}
            selectionKeyCode="Shift"
            multiSelectionKeyCode={null}
            panActivationKeyCode="Space"
            panOnDrag={[0, 1]}
            panOnScroll={false}
            zoomOnScroll
            zoomOnPinch
            zoomOnDoubleClick={false}
            minZoom={0.1}
            maxZoom={2.5}
            nodeDragThreshold={2}
            connectionRadius={24}
            proOptions={{ hideAttribution: true }}
            ariaLabelConfig={{
              "node.a11yDescription.default": "Press Enter or Space to select and edit it below. Arrow keys move it; moving changes the layout only.",
              "node.a11yDescription.keyboardDisabled": "Press Enter or Space to select.",
              "edge.a11yDescription.default": "Press Enter or Space to select this connection; Delete removes it.",
              "handle.ariaLabel": "Port: drag to a compatible port to connect",
            }}
          >
            <Controls onArrange={arrange} />
          </ReactFlow>
        </div>
        <ul className="xp-wfc__legend" aria-label="Legend">
          <li>
            <span className="xp-wfc__swatch" data-kind="data" aria-hidden="true" /> data (solid)
          </li>
          <li>
            <span className="xp-wfc__swatch" data-kind="prerequisite" aria-hidden="true" /> runs before, no data (dashed)
          </li>
          <li>
            <span className="xp-wfc__swatch" data-kind="annotation" aria-hidden="true" /> annotation, no data (dotted)
          </li>
          <li>Port shapes: ● data · ■ sources · ◆ evidence, reports, proposals</li>
        </ul>
      </div>
    </NodeContext.Provider>
  );
}

export function WorkflowCanvas(props: WorkflowCanvasProps) {
  return (
    <ReactFlowProvider>
      <CanvasInner {...props} />
    </ReactFlowProvider>
  );
}
