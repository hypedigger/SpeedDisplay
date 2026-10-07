import { defineConfig } from "vite";

// Tauri expects a fixed dev port.
export default defineConfig({
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: { target: "chrome120", outDir: "dist" },
});
