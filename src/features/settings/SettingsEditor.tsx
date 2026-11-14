// The Settings editor tab (`settings:` resource): a section list and the
// selected section. Each section reads and writes its owning service.
import type { ComponentType } from "react";
import { Bot, Cpu, FlaskConical, FolderCog, HardDrive, Info, Palette, ShieldCheck, SquarePen } from "lucide-react";

import { Tab, TabList, TabPanel, Tabs, type IconComponent } from "../../ui/components";
import { AboutSection } from "./AboutSection";
import { AgentsSection } from "./AgentsSection";
import { AppearanceSection } from "./AppearanceSection";
import { EditorSection } from "./EditorSection";
import { ExecutionSection } from "./ExecutionSection";
import { isSectionId, SECTIONS, type SectionId } from "./model";
import { ProjectSection } from "./ProjectSection";
import { ResearchSection } from "./ResearchSection";
import { showSection, useSettings } from "./store";
import { StorageSection } from "./StorageSection";
import { TrustSection } from "./TrustSection";
import "./settings.css";

const VIEWS: Record<SectionId, { icon: IconComponent; component: ComponentType }> = {
  appearance: { icon: Palette, component: AppearanceSection },
  editor: { icon: SquarePen, component: EditorSection },
  project: { icon: FolderCog, component: ProjectSection },
  trust: { icon: ShieldCheck, component: TrustSection },
  execution: { icon: Cpu, component: ExecutionSection },
  agents: { icon: Bot, component: AgentsSection },
  research: { icon: FlaskConical, component: ResearchSection },
  storage: { icon: HardDrive, component: StorageSection },
  about: { icon: Info, component: AboutSection },
};

export function SettingsEditor() {
  const section = useSettings((s) => s.section);
  return (
    <div className="exp-settings" data-testid="settings-view">
      <Tabs
        orientation="vertical"
        selectedKey={section}
        onSelectionChange={(key) => {
          if (isSectionId(key)) showSection(key);
        }}
        className="exp-settings__tabs"
      >
        <TabList aria-label="Settings sections" className="exp-settings__nav">
          {SECTIONS.map((s) => (
            <Tab key={s.id} id={s.id} icon={VIEWS[s.id].icon} className="exp-settings__nav-item">
              {s.title}
            </Tab>
          ))}
        </TabList>
        {SECTIONS.map((s) => {
          const View = VIEWS[s.id].component;
          return (
            <TabPanel key={s.id} id={s.id} className="exp-settings__panel">
              <View />
            </TabPanel>
          );
        })}
      </Tabs>
    </div>
  );
}
