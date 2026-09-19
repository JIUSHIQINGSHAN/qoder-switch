import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri 的窗口在 dev 下固定指向 1420，端口被占就报错而不是悄悄换端口。
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  envPrefix: ["VITE_", "TAURI_ENV_"],
  build: {
    target: "es2021",
    outDir: "dist",
  },
});
