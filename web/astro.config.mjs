// @ts-check
import { defineConfig } from "astro/config";
import react from "@astrojs/react";
import sitemap from "@astrojs/sitemap";
import tailwindcss from "@tailwindcss/vite";

// SITE_URL / BASE_PATH make the same build work on github.io/<repo> and on a custom domain.
const site = process.env.SITE_URL ?? "https://castrogusttavo.github.io";
const base = process.env.BASE_PATH ?? "/lumen-text";

export default defineConfig({
  site,
  base,
  output: "static",
  trailingSlash: "never",
  build: { format: "file", inlineStylesheets: "auto" },
  integrations: [react(), sitemap()],
  vite: { plugins: [tailwindcss()] },
  image: { layout: "constrained" },
});
