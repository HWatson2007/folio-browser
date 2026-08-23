import { defineConfig } from "vite";

// Multi-page setup: the profile picker and the browser chrome are separate
// Tauri webviews and need separate HTML entry points.
export default defineConfig({
  server: {
    port: 1420,
    strictPort: true,
  },
  build: {
    rollupOptions: {
      input: {
        main: "index.html",
        picker: "picker.html",
      },
    },
  },
});
