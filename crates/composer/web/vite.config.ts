import { defineConfig } from "vite";
import { fileURLToPath } from "node:url";

export default defineConfig({
  base: "./",
  build: { target: "es2022" },
  server: {
    fs: { allow: [fileURLToPath(new URL("../../../", import.meta.url))] },
  },
});
