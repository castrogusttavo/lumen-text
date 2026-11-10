import type { APIRoute } from "astro";
import { url } from "../lib/url";
export const GET: APIRoute = ({ site }) =>
  new Response(`User-agent: *\nAllow: /\n\nSitemap: ${new URL(url("/sitemap-index.xml"), site).href}\n`, { headers: { "content-type": "text/plain" } });
