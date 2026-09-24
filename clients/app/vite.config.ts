import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri 期望固定端口；Web 构建产物 dist/ 可直接部署为 PWA（连 nm-gateway）。
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: { target: "es2021", outDir: "dist" },
});
