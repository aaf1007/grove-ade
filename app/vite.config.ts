import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// The port and settings Tauri's devUrl expects.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  // A `?raw` import of a CSS file gives its text. Vitest empties CSS it does not process.
  test: { environment: "jsdom", css: { include: [/\.css\?raw$/] } },
});
