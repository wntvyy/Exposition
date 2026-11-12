// View filters (entity kinds, relationship kinds, inactive items) in a
// popover. Filters are part of the saved view; they never change the graph.
import { ListFilter } from "lucide-react";

import type { EntityKind } from "../../bindings/EntityKind";
import type { RelationKind } from "../../bindings/RelationKind";
import { Button, Checkbox, DialogTrigger, PopoverDialog } from "../../ui/components";
import { FAMILY_LABELS, type ClientFilters, type VocabularyIndex } from "./model";

const ENTITY_FAMILY_LABELS: Record<string, string> = {
  structure: "Structure",
  resource: "Resources",
  mathematics: "Mathematics",
  citation: "Citations",
  research: "Research",
  workflow: "Workflows",
};

function toggle<T>(all: T[], current: T[], item: T, on: boolean): T[] {
  const selected = current.length ? current : all;
  const next = on ? [...new Set([...selected, item])] : selected.filter((x) => x !== item);
  return next.length === all.length ? [] : next;
}

export function activeFilterCount(filters: ClientFilters): number {
  return (filters.kinds.length ? 1 : 0) + (filters.relationKinds.length ? 1 : 0) + (filters.showInactive ? 1 : 0);
}

export function FiltersButton({
  vocabulary,
  filters,
  onChange,
  entityKinds,
}: {
  vocabulary: VocabularyIndex | null;
  filters: ClientFilters;
  onChange: (patch: Partial<ClientFilters>) => void;
  /** Restrict the offered entity kinds (e.g. Mathematics). */
  entityKinds?: (kind: EntityKind) => boolean;
}) {
  const kinds = vocabulary ? [...vocabulary.entityKinds.values()].filter((k) => !entityKinds || entityKinds(k.kind)) : [];
  const relations = vocabulary ? [...vocabulary.relationKinds.values()] : [];
  const allKinds = kinds.map((k) => k.kind);
  const allRelations = relations.map((r) => r.kind);
  const families = [...new Set(kinds.map((k) => k.family))];
  const relationFamilies = [...new Set(relations.map((r) => r.family))];
  const count = activeFilterCount(filters);
  return (
    <DialogTrigger>
      <Button size="sm" variant="ghost" icon={ListFilter} aria-label={count ? `Filters (${count} active)` : "Filters"}>
        {count ? `Filters (${count})` : "Filters"}
      </Button>
      <PopoverDialog title="Filters" className="xp-graph-filters">
        <Checkbox isSelected={filters.showInactive} onChange={(on) => onChange({ showInactive: on })}>
          Show archived items and rejected suggestions
        </Checkbox>
        <fieldset className="xp-graph-filters__group">
          <legend>Item kinds</legend>
          {families.map((family) => (
            <div key={family} className="xp-graph-filters__family">
              <span className="xp-graph-filters__family-title">{ENTITY_FAMILY_LABELS[family] ?? family}</span>
              {kinds
                .filter((k) => k.family === family)
                .map((k) => (
                  <Checkbox
                    key={k.kind}
                    isSelected={!filters.kinds.length || filters.kinds.includes(k.kind)}
                    onChange={(on) => onChange({ kinds: toggle<EntityKind>(allKinds, filters.kinds, k.kind, on) })}
                  >
                    {k.title}
                  </Checkbox>
                ))}
            </div>
          ))}
          {filters.kinds.length > 0 && (
            <Button size="sm" variant="ghost" onPress={() => onChange({ kinds: [] })}>
              Show all kinds
            </Button>
          )}
        </fieldset>
        <fieldset className="xp-graph-filters__group">
          <legend>Relationships</legend>
          {relationFamilies.map((family) => (
            <div key={family} className="xp-graph-filters__family">
              <span className="xp-graph-filters__family-title">{FAMILY_LABELS[family]}</span>
              {relations
                .filter((r) => r.family === family)
                .map((r) => (
                  <Checkbox
                    key={r.kind}
                    isSelected={!filters.relationKinds.length || filters.relationKinds.includes(r.kind)}
                    onChange={(on) => onChange({ relationKinds: toggle<RelationKind>(allRelations, filters.relationKinds, r.kind, on) })}
                  >
                    {r.reading}
                  </Checkbox>
                ))}
            </div>
          ))}
          {filters.relationKinds.length > 0 && (
            <Button size="sm" variant="ghost" onPress={() => onChange({ relationKinds: [] })}>
              Show all relationships
            </Button>
          )}
        </fieldset>
      </PopoverDialog>
    </DialogTrigger>
  );
}
