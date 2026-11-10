// Converts selected HugeIcons Pro icons (JS data from an installed @hugeicons-pro package) to SVG
// files for tools/gen-icons.py.  usage: node hugeicons-to-svg.mjs <package dist/esm dir> <out dir>
import { readdirSync, writeFileSync, mkdirSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
import { join } from 'node:path';

const [dir, out] = process.argv.slice(2);
mkdirSync(out, { recursive: true });
// alias -> HugeIcons component name
const WANT = {
  settings: 'Settings01Icon', keyboard: 'KeyboardIcon', palette: 'PaintBoardIcon', search: 'Search01Icon', plus: 'Add01Icon',
  minus: 'MinusSignIcon', trash: 'Delete02Icon', copy: 'Copy01Icon', check: 'Tick02Icon', close: 'Cancel01Icon', refresh: 'RefreshIcon',
  undo: 'Undo02Icon', play: 'PlayIcon', pause: 'PauseIcon', stop: 'StopIcon', bug: 'Bug01Icon', terminal: 'ComputerTerminal01Icon',
  branch: 'GitBranchIcon', folder: 'Folder01Icon', file: 'File01Icon', chevron_right: 'ArrowRight01Icon', chevron_down: 'ArrowDown01Icon',
  chevron_up: 'ArrowUp01Icon', chevron_left: 'ArrowLeft01Icon', alert: 'Alert02Icon', info: 'InformationCircleIcon', error: 'AlertCircleIcon',
  sun: 'Sun03Icon', moon: 'Moon02Icon', edit: 'Edit02Icon', link: 'Link01Icon', menu: 'Menu01Icon', filter: 'FilterIcon',
  download: 'Download01Icon', upload: 'Upload01Icon', eye: 'ViewIcon', eye_off: 'ViewOffIcon', lock: 'LockIcon', star: 'StarIcon',
  bookmark: 'Bookmark01Icon', command: 'CommandIcon', code: 'SourceCodeIcon', save: 'FloppyDiskIcon', reset: 'ReloadIcon',
  text: 'TextFontIcon', zoom_in: 'ZoomInAreaIcon', zoom_out: 'ZoomOutAreaIcon', sidebar: 'SidebarLeftIcon', layout: 'LayoutTwoColumnIcon',
  keyboard_cmd: 'KeyboardIcon', globe: 'GlobalIcon', sparkles: 'SparklesIcon', flash: 'FlashIcon', bell: 'Notification01Icon',
  cpu: 'CpuIcon', more: 'MoreHorizontalIcon', arrow_left_right: 'ArrowLeftRightIcon', cursor: 'CursorTextIcon', tag: 'Tag01Icon', database: 'DatabaseIcon',
};
const files = new Set(readdirSync(dir));
const attr = { strokeWidth: 'stroke-width', strokeLinecap: 'stroke-linecap', strokeLinejoin: 'stroke-linejoin', fillRule: 'fill-rule', clipRule: 'clip-rule', strokeOpacity: 'stroke-opacity', fillOpacity: 'fill-opacity' };
let ok = 0, missing = [];
for (const [alias, comp] of Object.entries(WANT)) {
  const f = `${comp}.js`;
  if (!files.has(f)) { missing.push(`${alias}:${comp}`); continue; }
  const mod = await import(pathToFileURL(join(dir, f)).href);
  const data = mod.default;
  const body = data.map(([tag, a]) => {
    const parts = Object.entries(a).filter(([k]) => k !== 'key').map(([k, v]) => {
      let name = attr[k] || k;
      let val = String(v);
      if (val === 'currentColor') val = 'black';
      return `${name}="${val}"`;
    });
    return `<${tag} ${parts.join(' ')}/>`;
  }).join('');
  writeFileSync(join(out, `${alias}.svg`), `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none">${body}</svg>`);
  ok++;
}
console.log(`${ok} icons written; missing: ${missing.join(' ') || 'none'}`);
