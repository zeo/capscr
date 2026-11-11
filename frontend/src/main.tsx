import { render } from "solid-js/web";
import { App } from "./App";
import "./styles.css";

// text fields keep the native menu for cut, copy and paste; matched by type
// because number inputs report a null selectionStart
const TEXT_FIELDS = "textarea, input:not([type=checkbox], [type=radio], [type=range], [type=color], [type=button], [type=submit], [type=reset], [type=file], [type=image])";
document.addEventListener("contextmenu", (e) => {
  if (!(e.target instanceof Element && e.target.closest(TEXT_FIELDS))) e.preventDefault();
});

const root = document.getElementById("root");
if (!root) throw new Error("root element missing");
render(() => <App />, root);

const boot = document.getElementById("boot");
if (boot) boot.remove();
