// HugeIcons (free set, MIT: @hugeicons/core-free-icons) rendered to inline SVG at build time, so the page ships no icon JS.
// HugeIcons Pro is licensed and may not be redistributed, so it is never used on the public site.
import {
  AppleIcon, WindowsNewIcon, ComputerTerminal01Icon, Package01Icon, Download01Icon, Copy01Icon, Tick02Icon,
  ShieldKeyIcon, FileVerifiedIcon, ArrowRight01Icon, ArrowUpRight01Icon, GithubIcon, Calendar03Icon, CpuIcon,
  Layers01Icon, Key01Icon, LockKeyIcon, InformationCircleIcon, SparklesIcon, Archive01Icon, BoxIcon,
} from "@hugeicons/core-free-icons";

type Node = readonly [string, Record<string, string | number>];
const ICONS = {
  apple: AppleIcon, windows: WindowsNewIcon, terminal: ComputerTerminal01Icon, package: Package01Icon, download: Download01Icon,
  copy: Copy01Icon, check: Tick02Icon, shield: ShieldKeyIcon, verified: FileVerifiedIcon, arrow: ArrowRight01Icon,
  external: ArrowUpRight01Icon, github: GithubIcon, calendar: Calendar03Icon, cpu: CpuIcon, layers: Layers01Icon,
  key: Key01Icon, lock: LockKeyIcon, info: InformationCircleIcon, sparkles: SparklesIcon, archive: Archive01Icon, box: BoxIcon,
} as const satisfies Record<string, readonly Node[]>;
export type IconName = keyof typeof ICONS;

const kebab = (k: string) => k.replace(/[A-Z]/g, (c) => `-${c.toLowerCase()}`);
/** The SVG markup of a HugeIcons icon (24x24, currentColor strokes). */
export function icon(name: IconName, size = 20, cls = ""): string {
  const body = (ICONS[name] as readonly Node[])
    .map(([tag, attrs]) => `<${tag} ${Object.entries(attrs).filter(([k]) => k !== "key").map(([k, v]) => `${kebab(k)}="${v}"`).join(" ")}/>`)
    .join("");
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="${size}" height="${size}" fill="none" aria-hidden="true" focusable="false"${cls ? ` class="${cls}"` : ""}>${body}</svg>`;
}
