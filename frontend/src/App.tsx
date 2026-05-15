import { createSignal, Show } from "solid-js";
import { Settings } from "./views/Settings";
import { History } from "./views/History";
import { Destinations } from "./views/Destinations";
import { Marketplace } from "./views/Marketplace";

type Tab = "settings" | "history" | "destinations" | "marketplace";

export function App() {
  const [tab, setTab] = createSignal<Tab>("settings");

  return (
    <div class="app">
      <aside class="sidebar">
        <header>capscr</header>
        <div
          classList={{ tab: true, active: tab() === "settings" }}
          onClick={() => setTab("settings")}
        >
          Settings
        </div>
        <div
          classList={{ tab: true, active: tab() === "history" }}
          onClick={() => setTab("history")}
        >
          History
        </div>
        <div
          classList={{ tab: true, active: tab() === "destinations" }}
          onClick={() => setTab("destinations")}
        >
          Destinations
        </div>
        <div
          classList={{ tab: true, active: tab() === "marketplace" }}
          onClick={() => setTab("marketplace")}
        >
          Marketplace
        </div>
      </aside>
      <main class="content">
        <Show when={tab() === "settings"}>
          <Settings />
        </Show>
        <Show when={tab() === "history"}>
          <History />
        </Show>
        <Show when={tab() === "destinations"}>
          <Destinations />
        </Show>
        <Show when={tab() === "marketplace"}>
          <Marketplace />
        </Show>
      </main>
    </div>
  );
}
