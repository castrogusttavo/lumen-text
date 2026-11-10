const base = import.meta.env.BASE_URL.replace(/\/$/, "");
/** Prefix a site path with the configured base (works on github.io/<repo> and on a custom domain). */
export const url = (p = "/") => (p.startsWith("http") || p.startsWith("#") ? p : `${base}${p === "/" ? "" : p}` || "/");
export const REPO = "castrogusttavo/lumen-text";
export const GITHUB = `https://github.com/${REPO}`;
