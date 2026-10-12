// Pure functions behind the results screen: filtering, searching, sorting and the folder tree.
import type { FileKind } from './bindings/FileKind';
import type { FileStatus } from './bindings/FileStatus';
import type { FoundFile } from './bindings/FoundFile';
import type { VolumeRef } from './bindings/VolumeRef';
import { t } from './i18n';

export type ViewMode = 'list' | 'thumbnails';
export type KindFilter = 'all' | FileKind;
export type DeletedFilter = 'all' | 'deleted' | 'existing';
export type SizeFilter = 'all' | 'small' | 'medium' | 'large';
export type SortOrder = 'newest' | 'oldest' | 'name' | 'size';

/** What the sidebar points at: everything, one kind, or one format of a kind. */
export interface TypeScope {
  kind: KindFilter;
  /** A format code (`jpg`, `cr2`, ...), or null for every format of the kind. */
  format: string | null;
}

export interface Filters {
  type: TypeScope;
  /** Search text: names, folders, or extensions (`png`, `.cr2`, `*.jpg`). */
  query: string;
  deleted: DeletedFilter;
  size: SizeFilter;
  /** Capture (or file) year, e.g. `2024`, or null for any. */
  year: string | null;
  recoverableOnly: boolean;
  showHidden: boolean;
}

export const noFilters: Filters = {
  type: { kind: 'all', format: null },
  query: '',
  deleted: 'all',
  size: 'all',
  year: null,
  recoverableOnly: false,
  showHidden: false,
};

/** Size ranges of the size filter, in bytes: [from, to). */
const SIZES: Record<Exclude<SizeFilter, 'all'>, [number, number]> = {
  small: [0, 1_000_000],
  medium: [1_000_000, 10_000_000],
  large: [10_000_000, Number.POSITIVE_INFINITY],
};

/** Other spellings of format codes, as people type them. */
const ALIASES: Record<string, string> = { jpeg: 'jpg', jpe: 'jpg', tiff: 'tif', heif: 'heic', m2ts: 'mts', m4v: 'mp4' };

/** Every format code the scanner reports. */
export const FORMATS = new Set([
  'jpg', 'png', 'gif', 'bmp', 'tif', 'heic', 'webp',
  'cr2', 'cr3', 'nef', 'arw', 'dng', 'orf', 'rw2', 'pef', 'raf',
  'mp4', 'mov', 'avi', 'mts', '3gp',
]);

/** Camera raw formats, shown together under one label. */
export const RAW_FORMATS = new Set(['cr2', 'cr3', 'nef', 'arw', 'dng', 'orf', 'rw2', 'pef', 'raf']);

/** Display name of a format: `JPEG`, `PNG`, `CR2`, ... */
export function formatLabel(code: string): string {
  switch (code) {
    case 'jpg':
      return 'JPEG';
    case 'tif':
      return 'TIFF';
    case 'webp':
      return 'WebP';
    default:
      return code.toUpperCase();
  }
}

export function normalizeFormat(code: string): string {
  const c = code.toLowerCase();
  return ALIASES[c] ?? c;
}

export interface Query {
  /** Formats asked for by extension; empty for any. */
  formats: string[];
  /** Words that must appear in the name or folder path (lowercase). */
  words: string[];
}

/**
 * Splits search text into extensions and words. `.cr2` and `*.jpg` are extensions; a bare
 * word that is a known format (`png`) is an extension too.
 */
export function parseQuery(text: string): Query {
  const formats: string[] = [];
  const words: string[] = [];
  for (const raw of text.trim().toLowerCase().split(/\s+/)) {
    if (raw === '') continue;
    const ext = /^\*?\.([a-z0-9]+)$/.exec(raw)?.[1];
    if (ext !== undefined) {
      formats.push(normalizeFormat(ext));
    } else if (FORMATS.has(normalizeFormat(raw))) {
      formats.push(normalizeFormat(raw));
    } else {
      words.push(raw);
    }
  }
  return { formats, words };
}

/** The file's extension as written in its name (lowercase, normalized), if any. */
function nameExtension(file: FoundFile): string | null {
  const dot = file.name.lastIndexOf('.');
  return dot < 0 ? null : normalizeFormat(file.name.slice(dot + 1));
}

function matchesQuery(file: FoundFile, q: Query): boolean {
  if (q.formats.length > 0) {
    const ext = nameExtension(file);
    if (!q.formats.some((f) => f === file.format || f === ext)) return false;
  }
  if (q.words.length > 0) {
    const haystack = [file.name, ...(folderPath(file) ?? [])].join('/').toLowerCase();
    if (!q.words.every((w) => haystack.includes(w))) return false;
  }
  return true;
}

export function isRecoverable(status: FileStatus): boolean {
  return status === 'recoverable' || status === 'repaired';
}

/** Name shown for a volume: its label, or a numbered placeholder when it has none. */
export function volumeName(volume: VolumeRef): string {
  return volume.label ?? t('results.volume', { n: volume.index + 1 });
}

/** Folder path of a file found through the file system, starting with the volume name;
 *  null for carved files. */
export function folderPath(file: FoundFile): string[] | null {
  if (file.origin.type !== 'fileSystem') return null;
  return [volumeName(file.origin.volume), ...file.origin.folders];
}

/** The file's date as an ISO local date-time, whatever its source. */
export function dateOf(file: FoundFile): string | null {
  return file.date?.local ?? null;
}

export function inType(file: FoundFile, type: TypeScope): boolean {
  return (type.kind === 'all' || file.kind === type.kind) && (type.format === null || file.format === type.format);
}

/** Every filter except the sidebar's type (for the sidebar's own counts). */
export function passesOthers(file: FoundFile, f: Filters, q: Query = parseQuery(f.query)): boolean {
  if (!f.showHidden && file.hidden !== null) return false;
  if (f.deleted === 'deleted' && !file.deleted) return false;
  if (f.deleted === 'existing' && file.deleted) return false;
  if (f.size !== 'all') {
    const [from, to] = SIZES[f.size];
    if (file.size < from || file.size >= to) return false;
  }
  if (f.year !== null && dateOf(file)?.slice(0, 4) !== f.year) return false;
  if (f.recoverableOnly && !isRecoverable(file.status)) return false;
  return matchesQuery(file, q);
}

export function applyFilters(files: FoundFile[], f: Filters): FoundFile[] {
  const q = parseQuery(f.query);
  return files.filter((file) => inType(file, f.type) && passesOthers(file, f, q));
}

/** Whether any filter narrows the list (for the reset button). */
export function isFiltered(f: Filters): boolean {
  return (
    f.type.kind !== 'all' ||
    f.query.trim() !== '' ||
    f.deleted !== 'all' ||
    f.size !== 'all' ||
    f.year !== null ||
    f.recoverableOnly
  );
}

/** Years files were taken (or written) in, newest first. */
export function yearsOf(files: FoundFile[]): string[] {
  const years = new Set<string>();
  for (const file of files) {
    const y = dateOf(file)?.slice(0, 4);
    if (y) years.add(y);
  }
  return [...years].sort().reverse();
}

export interface FormatCount {
  format: string;
  count: number;
}

export interface TypeCounts {
  all: number;
  photo: number;
  video: number;
  /** Per kind, its formats by count (most first). */
  formats: Record<FileKind, FormatCount[]>;
}

export function typeCounts(files: FoundFile[]): TypeCounts {
  const per: Record<FileKind, Map<string, number>> = { photo: new Map(), video: new Map() };
  for (const file of files) per[file.kind].set(file.format, (per[file.kind].get(file.format) ?? 0) + 1);
  const list = (m: Map<string, number>) =>
    [...m.entries()].map(([format, count]) => ({ format, count })).sort((a, b) => b.count - a.count);
  const total = (m: Map<string, number>) => [...m.values()].reduce((a, b) => a + b, 0);
  return {
    all: files.length,
    photo: total(per.photo),
    video: total(per.video),
    formats: { photo: list(per.photo), video: list(per.video) },
  };
}

const collator = new Intl.Collator(undefined, { numeric: true });

function byDate(a: FoundFile, b: FoundFile, dir: 1 | -1): number {
  // Files without a date go last in both directions.
  const da = dateOf(a);
  const db = dateOf(b);
  if (da === db) return collator.compare(a.name, b.name);
  if (da === null) return 1;
  if (db === null) return -1;
  return da < db ? -dir : dir;
}

export function compareFiles(order: SortOrder): (a: FoundFile, b: FoundFile) => number {
  switch (order) {
    case 'newest':
      return (a, b) => byDate(a, b, -1);
    case 'oldest':
      return (a, b) => byDate(a, b, 1);
    case 'name':
      return (a, b) => collator.compare(a.name, b.name);
    case 'size':
      return (a, b) => b.size - a.size || collator.compare(a.name, b.name);
  }
}

export function sortFiles(files: FoundFile[], order: SortOrder): FoundFile[] {
  return [...files].sort(compareFiles(order));
}

/** A folder in the results tree: a volume, a folder in it, or the carved files. */
export interface FolderNode {
  key: string;
  name: string;
  /** Files in it and below. */
  count: number;
  size: number;
  folders: FolderNode[];
  files: FoundFile[];
  /** Holds the files found in the raw data (no names, no folders). */
  carved: boolean;
}

/** Key of the folder holding the carved files. */
export const CARVED_KEY = 'carved';

/**
 * The folder tree of `files`: volumes with their folders, then a folder for the carved
 * files. Folders are in name order, files in `order`; counts and sizes include subfolders.
 */
export function buildTree(files: FoundFile[], order: SortOrder): FolderNode[] {
  const roots = new Map<string, FolderNode>();
  const node = (siblings: Map<string, FolderNode>, key: string, name: string, carved = false) => {
    let n = siblings.get(name);
    if (n === undefined) {
      n = { key, name, count: 0, size: 0, folders: [], files: [], carved };
      siblings.set(name, n);
    }
    return n;
  };
  // Children by name while building; turned into sorted arrays at the end.
  const children = new Map<FolderNode, Map<string, FolderNode>>();
  const kids = (n: FolderNode) => {
    let m = children.get(n);
    if (m === undefined) {
      m = new Map();
      children.set(n, m);
    }
    return m;
  };
  let carved: FolderNode | null = null;
  for (const file of files) {
    const path = folderPath(file);
    let at: FolderNode;
    if (path === null) {
      carved ??= { key: CARVED_KEY, name: t('results.carvedFolder'), count: 0, size: 0, folders: [], files: [], carved: true };
      at = carved;
      at.count += 1;
      at.size += file.size;
    } else {
      let siblings = roots;
      let key = '';
      at = undefined as unknown as FolderNode;
      for (const name of path) {
        key += `/${name}`;
        at = node(siblings, key, name);
        at.count += 1;
        at.size += file.size;
        siblings = kids(at);
      }
    }
    at.files.push(file);
  }
  const compare = compareFiles(order);
  const finish = (n: FolderNode) => {
    n.folders = [...(children.get(n)?.values() ?? [])].sort((a, b) => collator.compare(a.name, b.name));
    n.folders.forEach(finish);
    n.files.sort(compare);
  };
  const out = [...roots.values()].sort((a, b) => collator.compare(a.name, b.name));
  out.forEach(finish);
  if (carved) {
    finish(carved);
    out.push(carved);
  }
  return out;
}

/** A row of the tree table. */
export type Row =
  | { type: 'folder'; node: FolderNode; depth: number; open: boolean }
  | { type: 'file'; file: FoundFile; depth: number };

/** The rows shown with the folders in `open` expanded. */
export function flattenTree(roots: FolderNode[], open: ReadonlySet<string>): Row[] {
  const rows: Row[] = [];
  const walk = (n: FolderNode, depth: number) => {
    const isOpen = open.has(n.key);
    rows.push({ type: 'folder', node: n, depth, open: isOpen });
    if (!isOpen) return;
    for (const f of n.folders) walk(f, depth + 1);
    for (const file of n.files) rows.push({ type: 'file', file, depth: depth + 1 });
  };
  roots.forEach((r) => walk(r, 0));
  return rows;
}

/** Ids of every file in a folder and below. */
export function filesUnder(n: FolderNode): number[] {
  const out: number[] = [];
  const walk = (m: FolderNode) => {
    for (const f of m.files) out.push(f.id);
    m.folders.forEach(walk);
  };
  walk(n);
  return out;
}
