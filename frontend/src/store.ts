import { createResource } from "solid-js";
import { api, AppConfig } from "./api";

export const [config, { mutate: mutateConfig, refetch: refetchConfig }] = createResource<AppConfig>(api.getConfig);
