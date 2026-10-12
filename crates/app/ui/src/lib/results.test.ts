import { describe, expect, it } from 'vitest';
import type { FoundFile } from './bindings/FoundFile';
import { t } from './i18n';
import {
  CARVED_KEY,
  applyFilters,
  buildTree,
  filesUnder,
  flattenTree,
  noFilters,
  parseQuery,
  sortFiles,
  typeCounts,
  yearsOf,
  type Filters,
} from './results';

let nextId = 0;

function fsOrigin(folders: string[], label: string | null = 'SD'): FoundFile['origin'] {
  return { type: 'fileSystem', volume: { index: 0, label }, folders, nameExact: true };
}
function file(over: Partial<FoundFile>): FoundFile {
  nextId += 1;
  return {
    id: nextId,
    kind: 'photo',
    format: 'jpg',
    name: `IMG_${nextId}.JPG`,
    origin: fsOrigin(['DCIM', '100CANON']),
    deleted: true,
    width: null,
    height: null,
    offset: 0,
    size: 1000,
    date: { local: '2025-08-14T10:00:00', source: 'capture' },
    durationSeconds: null,
    confidence: null,
    status: 'recoverable',
    hidden: null,
    repair: null,
    ...over,
  };
}

const f = (over: Partial<Filters>): Filters => ({ ...noFilters, ...over });

describe('search', () => {
  it('reads extensions and words', () => {
    expect(parseQuery('png')).toEqual({ formats: ['png'], words: [] });
    expect(parseQuery('.CR2 *.jpeg')).toEqual({ formats: ['cr2', 'jpg'], words: [] });
    expect(parseQuery(' holiday  2024 ')).toEqual({ formats: [], words: ['holiday', '2024'] });
  });

  it('finds files by extension, name and folder', () => {
    const files = [
      file({ name: 'beach.JPG' }),
      file({ name: 'icon.png', format: 'png', origin: fsOrigin(['Windows', 'Icons']) }),
      file({ name: 'IMG_0001.CR2', format: 'cr2' }),
    ];
    const names = (q: string) => applyFilters(files, f({ query: q })).map((x) => x.name);
    expect(names('png')).toEqual(['icon.png']);
    expect(names('.cr2')).toEqual(['IMG_0001.CR2']);
    expect(names('*.jpg')).toEqual(['beach.JPG']);
    expect(names('beach')).toEqual(['beach.JPG']);
    expect(names('icons')).toEqual(['icon.png']);
  });
});

describe('filters', () => {
  it('hides junk unless asked, and filters by type, state and size', () => {
    const files = [
      file({}),
      file({ hidden: 'tooSmall' }),
      file({ kind: 'video', format: 'mp4' }),
      file({ status: 'partiallyDamaged', deleted: false }),
      file({ size: 20_000_000 }),
    ];
    expect(applyFilters(files, noFilters)).toHaveLength(4);
    expect(applyFilters(files, f({ showHidden: true }))).toHaveLength(5);
    expect(applyFilters(files, f({ type: { kind: 'video', format: null } }))).toHaveLength(1);
    expect(applyFilters(files, f({ type: { kind: 'photo', format: 'mp4' } }))).toHaveLength(0);
    expect(applyFilters(files, f({ recoverableOnly: true }))).toHaveLength(3);
    expect(applyFilters(files, f({ deleted: 'existing' }))).toHaveLength(1);
    expect(applyFilters(files, f({ deleted: 'deleted' }))).toHaveLength(3);
    expect(applyFilters(files, f({ size: 'large' }))).toHaveLength(1);
  });

  it('filters by year and lists the years', () => {
    const files = [file({}), file({ date: { local: '2023-01-02T00:00:00', source: 'capture' } }), file({ date: null })];
    expect(yearsOf(files)).toEqual(['2025', '2023']);
    expect(applyFilters(files, f({ year: '2023' }))).toHaveLength(1);
  });

  it('counts types and formats for the sidebar', () => {
    const counts = typeCounts([
      file({}),
      file({}),
      file({ format: 'png' }),
      file({ kind: 'video', format: 'mts' }),
    ]);
    expect(counts.all).toBe(4);
    expect(counts.photo).toBe(3);
    expect(counts.formats.photo).toEqual([
      { format: 'jpg', count: 2 },
      { format: 'png', count: 1 },
    ]);
    expect(counts.formats.video).toEqual([{ format: 'mts', count: 1 }]);
  });
});

describe('sorting', () => {
  it('puts undated files last', () => {
    const a = file({ date: { local: '2025-08-14T10:00:00', source: 'capture' } });
    const b = file({ date: null });
    const c = file({ date: { local: '2025-08-15T10:00:00', source: 'capture' } });
    expect(sortFiles([a, b, c], 'newest').map((x) => x.id)).toEqual([c.id, a.id, b.id]);
    expect(sortFiles([a, b, c], 'oldest').map((x) => x.id)).toEqual([a.id, c.id, b.id]);
  });
});

describe('tree', () => {
  it('nests folders with counts and sizes, and the carved files last', () => {
    const files = [
      file({ origin: fsOrigin(['DCIM', '100CANON']), size: 10 }),
      file({ origin: fsOrigin(['DCIM', '101CANON']), size: 20 }),
      file({ origin: fsOrigin([]), size: 5 }),
      file({ origin: { type: 'carved', namedFromMetadata: false }, size: 7 }),
    ];
    const roots = buildTree(files, 'name');
    expect(roots.map((r) => r.name)).toEqual(['SD', t('results.carvedFolder')]);
    const sd = roots[0]!;
    expect([sd.count, sd.size]).toEqual([3, 35]);
    expect(sd.files).toHaveLength(1);
    expect(sd.folders.map((x) => x.name)).toEqual(['DCIM']);
    expect(sd.folders[0]!.folders.map((x) => [x.name, x.count])).toEqual([
      ['100CANON', 1],
      ['101CANON', 1],
    ]);
    expect(roots[1]!.key).toBe(CARVED_KEY);
    expect(filesUnder(sd)).toHaveLength(3);
  });

  it('shows only the open folders', () => {
    const files = [file({ origin: fsOrigin(['DCIM']) }), file({ origin: fsOrigin(['DCIM']) })];
    const roots = buildTree(files, 'name');
    const closed = flattenTree(roots, new Set());
    expect(closed.map((r) => r.type)).toEqual(['folder']);
    const open = flattenTree(roots, new Set(['/SD', '/SD/DCIM']));
    expect(open.map((r) => [r.type, r.depth])).toEqual([
      ['folder', 0],
      ['folder', 1],
      ['file', 2],
      ['file', 2],
    ]);
  });
});
