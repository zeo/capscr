import { render } from "solid-js/web";
import { App } from "./App";
import "./styles.css";

// text fields keep the native menu for cut, copy and paste
document.addEventListener("contextmenu", (e) => {
  const field = e.target instanceof Element ? e.target.closest("textarea, input") : null;
  if (!(field instanceof HTMLTextAreaElement || (field instanceof HTMLInputElement && field.selectionStart !== null))) {
    e.preventDefault();
  }
});

const root = document.getElementById("root");
if (!root) throw new Error("root element missing");
render(() => <App />, root);

const boot = document.getElementById("boot");
if (boot) boot.remove();
