// Page images arrive as raw bytes (SVG from Typst, PNG from TeX adapters).

/** Media type of a page image from its leading bytes. */
export function imageType(bytes: Uint8Array): string {
  if (bytes[0] === 0x89 && bytes[1] === 0x50 && bytes[2] === 0x4e && bytes[3] === 0x47) return "image/png";
  if (bytes[0] === 0xff && bytes[1] === 0xd8) return "image/jpeg";
  return "image/svg+xml";
}
