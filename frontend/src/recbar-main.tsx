import { render } from "solid-js/web";
import { RecBar } from "./views/RecBar";
import "./styles.css";

document.addEventListener("contextmenu", (e) => e.preventDefault());

const root = document.getElementById("root");
if (!root) throw new Error("root element missing");
render(() => <RecBar />, root);
