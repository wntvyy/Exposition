// Public API of the source editor for other features (e.g. page editing).
// Import from "src/features/editor"; registration lives in ./register.
export { SourceEditor, type SourceEditorProps } from "./SourceEditor";
export {
  activeHandle,
  attachDocument,
  fileKey,
  handleFor,
  handleForPath,
  MIXED_EOL_REASON,
  normalizeLineEndings,
  openDocument,
  openHandles,
  reloadDocument,
  saveAll,
  saveDocument,
  type DocumentHandle,
} from "./documentClient";
