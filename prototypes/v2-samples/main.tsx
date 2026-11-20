// V2 UI samples: standalone harness for click-through and screenshots. It
// renders the same module as the in-app gallery (src/ui/reference/v2).
// URL parameters: see `parseSampleParams` (e.g. ?s=balanced&view=agent&w=360;
// chrome=0 hides the review bar for screenshots).
import "../../src/ui/tokens.css";

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { parseSampleParams, sampleSearch, V2Samples } from "../../src/ui/reference/v2/V2Samples";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <V2Samples initial={parseSampleParams(location.search)} onChange={(p) => history.replaceState(null, "", sampleSearch(p))} />
  </StrictMode>,
);
