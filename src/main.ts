import { mount } from "svelte";
import App from "./App.svelte";
import { applyFontDelta, loadFontDelta } from "./lib/fontSize";
import { applyTheme, loadTheme } from "./lib/theme";
import "./styles.css";

applyTheme(loadTheme());
applyFontDelta(loadFontDelta(), false);

const target = document.getElementById("app");
if (!target) throw new Error("#app element missing");

export default mount(App, { target });
