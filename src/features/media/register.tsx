// Media contributions: the read-only image editor tab for project images and
// inline image previews in Markdown/LaTeX/Typst source.
import { registry } from "../../app/registry";
import { fileExtension } from "../../ui/components";
import { registerSourceEditorContribution } from "../editor/contributions";
import { ImageViewer } from "./ImageViewer";
import { IMAGE_TAB_EXTENSIONS } from "./loader";
import { activePreviewController, attachImagePreviews } from "./sourcePreviews";

registry.registerEditor({
  id: "media.image",
  // Above the source editor (an image is never opened as an editable text or
  // binary buffer) and the document editors.
  priority: 20,
  matches: (resource) =>
    resource.key.startsWith("file:") && Boolean(resource.path) && IMAGE_TAB_EXTENSIONS.has(fileExtension(resource.path!)),
  modes: ["source"],
  component: ImageViewer,
});

registerSourceEditorContribution("media.imagePreviews", attachImagePreviews);

registry.registerCommand({
  id: "media.showSourceImagePreviews",
  title: "Show Image Previews in Source",
  category: "View",
  enabled: () => activePreviewController()?.enabled === false,
  run: () => activePreviewController()?.setDocumentPreviews(true),
});

registry.registerCommand({
  id: "media.hideSourceImagePreviews",
  title: "Hide Image Previews in Source",
  category: "View",
  enabled: () => activePreviewController()?.enabled === true,
  run: () => activePreviewController()?.setDocumentPreviews(false),
});

registry.registerCommand({
  id: "media.toggleSourceImagePreviewAtCursor",
  title: "Show or Hide Image Preview at Cursor",
  category: "View",
  enabled: () => activePreviewController()?.enabled === true,
  run: () => void activePreviewController()?.toggleAtCursor(),
});
