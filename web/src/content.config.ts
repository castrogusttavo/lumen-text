import { defineCollection, z } from "astro:content";
import { glob } from "astro/loaders";

const docs = defineCollection({
  loader: glob({ pattern: "**/*.md", base: "./src/content/docs" }),
  schema: z.object({
    title: z.string(),
    description: z.string(),
    section: z.enum(["Start", "Using Lumen", "Customize", "Project"]),
    order: z.number(),
    /** Extra interactive visual rendered under the intro (see pages/docs/[...slug].astro). */
    visual: z.enum(["tour", "git", "split", "themes", "palette", "settings"]).optional(),
    /** A real screenshot (public/img/shot-<name>) shown at the top. */
    shot: z.enum(["git", "md", "split", "palette"]).optional(),
    /** Generated reference table appended after the text. */
    generated: z.enum(["shortcuts", "commands", "settings"]).optional(),
  }),
});

export const collections = { docs };
