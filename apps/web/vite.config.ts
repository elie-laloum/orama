import { defineConfig } from "vite";
import react from "@vitejs/plugin-react-swc";
import tailwindcss from "@tailwindcss/vite";
import path from "node:path";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: { alias: { "@": path.resolve(__dirname, "src") } },
  // The Rust binary serves the bundle from /ui.
  base: "/ui/",
  build: {
    outDir: "dist",
    sourcemap: true,
    target: "es2022",
    rollupOptions: {
      // Fixed filenames: `core/build.rs` embeds these two paths, and a content
      // hash would break that on every build.
      output: {
        entryFileNames: "main.js",
        chunkFileNames: "main.js",
        assetFileNames: "main[extname]",
      },
    },
  },
  server: {
    port: 5173,
    proxy: {
      "/api": { target: "http://127.0.0.1:8787", changeOrigin: true },
    },
  },
});
