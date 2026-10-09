import { mount } from "svelte";
import App from "./App.svelte";
import { applyTheme, loadTheme } from "./lib/theme";
import "./styles.css";

applyTheme(loadTheme());

const target = document.getElementById("app");
if (!target) throw new Error("#app element missing");

export default mount(App, { target });
