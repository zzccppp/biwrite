import { createReadStream, cpSync, existsSync, statSync } from "node:fs";
import { resolve, sep } from "node:path";
import { defineConfig, type Plugin } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

const host = process.env.TAURI_DEV_HOST;

/** pdf.js loads character maps, standard fonts and image decoders at run
 * time from /pdfjs/: served from node_modules in development, copied into
 * the build otherwise. */
function pdfjsAssets(): Plugin {
  const root = resolve("node_modules/pdfjs-dist");
  const dirs = ["cmaps", "standard_fonts", "wasm"];
  const types: Record<string, string> = { wasm: "application/wasm", js: "text/javascript", bcmap: "application/octet-stream" };
  return {
    name: "biwrite-pdfjs-assets",
    configureServer(server) {
      server.middlewares.use("/pdfjs", (req, res, next) => {
        const path = resolve(root, `.${decodeURIComponent((req.url ?? "").split("?")[0])}`);
        const allowed = dirs.some((d) => path.startsWith(resolve(root, d) + sep));
        if (!allowed || !existsSync(path) || !statSync(path).isFile()) return next();
        const ext = path.split(".").pop() ?? "";
        res.setHeader("Content-Type", types[ext] ?? "application/octet-stream");
        createReadStream(path).pipe(res);
      });
    },
    writeBundle(options) {
      const out = resolve(options.dir ?? "dist", "pdfjs");
      for (const d of dirs) cpSync(resolve(root, d), resolve(out, d), { recursive: true });
    },
  };
}

// https://v2.tauri.app/start/frontend/vite/
export default defineConfig({
  plugins: [svelte(), pdfjsAssets()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 1421 } : undefined,
    watch: { ignored: ["**/src-tauri/**", "**/crates/**", "**/target/**"] },
  },
  envPrefix: ["VITE_", "TAURI_ENV_*"],
  build: {
    // WebKit on macOS, WebView2 (Chromium) on Windows.
    target: process.env.TAURI_ENV_PLATFORM === "windows" ? "chrome110" : "safari16",
    minify: !process.env.TAURI_ENV_DEBUG,
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
    // A desktop bundle loaded from disk; CodeMirror alone is ~500 kB, pdf.js ~400 kB.
    chunkSizeWarningLimit: 1500,
  },
});
