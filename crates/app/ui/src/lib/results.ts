// Pure functions behind the results screen: filtering, sorting, grouping and the folder tree.
import type { FileKind } from './bindings/FileKind';
import type { FileStatus } from './bindings/FileStatus';
import type { FoundFile } from './bindings/FoundFile';

/** Which part of the results the sidebar points at. */
export type Scope =
  | { type: 'all' }
  | { type: 'folder'; path: readonly string[] }
  | { type: 'carved'; kind: FileKind };

export type ViewMode = 'folder' | 'thumbnails' | 'list';
export type KindFilter = 'all' | FileKind;
export type SortOrder = 'newest' | 'oldest' | 'name' | 'size';

export interface Filters {
  scope: Scope;
  kind: KindFilter;
  recoverableOnly: boolean;
  showHidden: boolean;
}

export function isRecoverable(status: FileStatus): boolean {
  return status === 'recoverable' || status === 'repaired';
}

function startsWith(folders: readonly string[], prefix: readonly string[]): boolean {
  return prefix.length <= folders.length && prefix.every((name, i) => folders[i] === name);
}

/** `deep`: include files in subfolders (thumbnails and list) or only direct children (folder view). */
export function inScope(file: FoundFile, scope: Scope, deep = true): boolean {
  switch (scope.type) {
    case 'all':
      return true;
    case 'folder':
      return (
        file.origin.type === 'fileSystem' &&
        startsWith(file.origin.folders, scope.path) &&
        (deep || file.origin.folders.length === scope.path.length)
      );
    case 'carved':
      return file.origin.type === 'carved' && file.kind === scope.kind;
  }
}

/** Filters that do not depend on where the user is in the folder hierarchy. */
export function passesFilters(file: FoundFile, f: Omit<Filters, 'scope'>): boolean {
  return (
    (f.showHidden || file.hidden === null) &&
    (f.kind === 'all' || file.kind === f.kind) &&
    (!f.recoverableOnly || isRecoverable(file.status))
  );
}

export function applyFilters(files: FoundFile[], f: Filters, deep = true): FoundFile[] {
  return files.filter((file) => passesFilters(file, f) && inScope(file, f.scope, deep));
}

const collator = new Intl.Collator(undefined, { numeric: true });

function byCapture(a: FoundFile, b: FoundFile, dir: 1 | -1): number {
  // Files without a capture date go last in both directions.
  if (a.capturedAt === b.capturedAt) return collator.compare(a.name, b.name);
  if (a.capturedAt === null) return 1;
  if (b.capturedAt === null) return -1;
  return a.capturedAt < b.capturedAt ? -dir : dir;
}

export function sortFiles(files: FoundFile[], order: SortOrder): FoundFile[] {
  const out = [...files];
  switch (order) {
    case 'newest':
      return out.sort((a, b) => byCapture(a, b, -1));
    case 'oldest':
      return out.sort((a, b) => byCapture(a, b, 1));
    case 'name':
      return out.sort((a, b) => collator.compare(a.name, b.name));
    case 'size':
      return out.sort((a, b) => b.size - a.size || collator.compare(a.name, b.name));
  }
}

/** Where the files of a group came from, for its subtitle. */
export type GroupSource =
  | { type: 'folder'; folders: string[] }
  | { type: 'carved'; namedFromMetadata: boolean };

export interface Group {
  key: string;
  /** `YYYY-MM-DD`, or null for files without a capture date. */
  day: string | null;
  source: GroupSource;
  files: FoundFile[];
}

/** Groups sorted files by capture day, then by origin, keeping the given order. */
export function groupByDay(sorted: FoundFile[]): Group[] {
  const groups = new Map<string, Group>();
  for (const file of sorted) {
    const day = file.capturedAt?.slice(0, 10) ?? null;
    const source: GroupSource =
      file.origin.type === 'fileSystem'
        ? { type: 'folder', folders: file.origin.folders }
        : { type: 'carved', namedFromMetadata: file.origin.namedFromMetadata };
    const sourceKey =
      source.type === 'folder' ? `f:${source.folders.join('/')}` : `c:${source.namedFromMetadata}`;
    const key = `${day ?? '-'}|${sourceKey}`;
    let group = groups.get(key);
    if (group === undefined) {
      group = { key, day, source, files: [] };
      groups.set(key, group);
    }
    group.files.push(file);
  }
  return [...groups.values()];
}

export interface FolderRow {
  path: string[];
  name: string;
  depth: number;
  /** Files in this folder and its subfolders. */
  count: number;
}

/** Every folder that holds files, depth-first in name order, with file counts. */
export function folderTree(files: FoundFile[]): FolderRow[] {
  const counts = new Map<string, { path: string[]; count: number }>();
  for (const file of files) {
    if (file.origin.type !== 'fileSystem') continue;
    const folders = file.origin.folders;
    for (let depth = 1; depth <= folders.length; depth++) {
      const path = folders.slice(0, depth);
      const key = path.join('\u0000');
      const entry = counts.get(key);
      if (entry) entry.count += 1;
      else counts.set(key, { path, count: 1 });
    }
  }
  return [...counts.entries()]
    .sort(([a], [b]) => collator.compare(a, b))
    .map(([, { path, count }]) => ({
      path,
      name: path[path.length - 1] ?? '',
      depth: path.length - 1,
      count,
    }));
}

/** Direct subfolders of `path` (`[]` for the top level), for the folder view. */
export function subfolders(files: FoundFile[], path: readonly string[]): FolderRow[] {
  return folderTree(files).filter(
    (row) => row.path.length === path.length + 1 && startsWith(row.path, path),
  );
}

export function countCarved(files: FoundFile[], kind: FileKind): number {
  return files.filter((f) => f.origin.type === 'carved' && f.kind === kind).length;
}

export function sameScope(a: Scope, b: Scope): boolean {
  if (a.type === 'folder' && b.type === 'folder') return a.path.join('/') === b.path.join('/');
  if (a.type === 'carved' && b.type === 'carved') return a.kind === b.kind;
  return a.type === b.type;
}
