import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import path from "node:path";

// Tauri 的窗口在 dev 下固定指向 1420，端口被占就报错而不是悄悄换端口。
// @ 别名与 Tailwind v4 是复刻版前端的接线前提；本文件不在 tsconfig.include 内，
// 所以不必为 node 的类型再引一个 @types/node。
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": path.resolve(import.meta.dirname, "./src"),
    },
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**", "**/crates/**"] },
  },
  envPrefix: ["VITE_", "TAURI_ENV_"],
  build: {
    target: "es2021",
    outDir: "dist",
  },
});
