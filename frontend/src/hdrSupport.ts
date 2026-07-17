import { createResource } from "solid-js";
import { api } from "./api";
import { IS_LINUX } from "./keys";

// whether this machine can produce hdr captures at all: true on windows,
// resolved from the backend on linux (gnome-on-wayland only). windows keeps
// its panes during the async resolve; linux reveals them once confirmed.
const [supported] = createResource(() => api.hdrPipelineSupported(), {
  initialValue: !IS_LINUX,
});

export const hdrSupported = supported;
