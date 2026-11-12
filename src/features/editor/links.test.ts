import { afterEach, describe, expect, it, vi } from "vitest";

import { dismiss, useNotifications } from "../../app/notifications";
import { offerEditorLink } from "./links";

afterEach(() => {
  for (const n of useNotifications.getState().items) dismiss(n.id);
});

describe("offerEditorLink", () => {
  it("copies the link and says so, without opening anything", async () => {
    const open = vi.spyOn(window, "open").mockImplementation(() => null);
    const before = window.location.href;
    const writeText = vi.fn(async () => {});
    await offerEditorLink("https://example.com/docs?q=1", { writeText });
    expect(writeText).toHaveBeenCalledWith("https://example.com/docs?q=1");
    expect(open).not.toHaveBeenCalled();
    expect(window.location.href).toBe(before);
    const [note] = useNotifications.getState().items;
    expect(note?.title).toBe("Link copied");
    expect(note?.body).toContain("https://example.com/docs?q=1");
    open.mockRestore();
  });

  it("reports the link when the clipboard is unavailable", async () => {
    await offerEditorLink("https://example.com", { writeText: async () => Promise.reject(new Error("denied")) });
    expect(useNotifications.getState().items[0]?.title).toBe("Link not opened");
    await offerEditorLink("x".repeat(500), undefined);
    expect(useNotifications.getState().items.at(-1)?.body?.length).toBeLessThan(300);
  });
});
