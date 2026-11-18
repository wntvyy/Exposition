// Open the Settings editor (`settings:` resource), optionally at a section.
// Side-effect free, so other features can open a section without loading the
// settings registrations.
import { editors } from "../../app/editors";
import { SETTINGS_RESOURCE, type SectionId } from "./model";
import { showSection } from "./store";

export function openSettings(section?: SectionId): void {
  if (section) showSection(section);
  editors.open({ ...SETTINGS_RESOURCE });
}
