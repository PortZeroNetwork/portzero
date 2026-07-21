import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri embeds the built assets from `dist/` into the app binary, so we build
// with relative asset URLs (base "./") and emit into `dist`. The dev server
// port matches `devUrl` in tauri.conf.json.
export default defineConfig({
  plugins: [react()],
  base: "./",
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    target: "es2020",
  },
});
