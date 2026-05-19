import { render } from "solid-js/web";
import { App } from "./App";
import "./styles.css";

const root = document.getElementById("root");
if (!root) throw new Error("root element missing");
render(() => <App />, root);

const boot = document.getElementById("boot");
if (boot) boot.remove();
