import { describe, expect, it } from 'vitest';
import type { FoundFile } from './bindings/FoundFile';
import { t } from './i18n';
import { applyFilters, folderTree, groupByDay, sortFiles, subfolders } from './results';

let nextId = 0;

function fsOrigin(folders: string[], label: string | null = 'SD'): FoundFile['origin'] {
  return { type: 'fileSystem', volume: { index: 0, label }, folders, nameExact: true };
}
function file(over: Partial<FoundFile>): FoundFile {
  return {
    id: nextId++,
    kind: 'photo',
    name: `IMG_${nextId}.JPG`,
    origin: fsOrigin(['DCIM', '100CANON']),
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

const base = { kind: 'all', recoverableOnly: false, showHidden: false } as const;

describe('results', () => {
  it('hides junk unless asked, and filters by kind and status', () => {
    const files = [
      file({}),
      file({ hidden: 'tooSmall' }),
      file({ kind: 'video' }),
      file({ status: 'partiallyDamaged' }),
    ];
    expect(applyFilters(files, { ...base, scope: { type: 'all' } })).toHaveLength(3);
    expect(applyFilters(files, { ...base, showHidden: true, scope: { type: 'all' } })).toHaveLength(4);
    expect(applyFilters(files, { ...base, kind: 'video', scope: { type: 'all' } })).toHaveLength(1);
    expect(
      applyFilters(files, { ...base, recoverableOnly: true, scope: { type: 'all' } }),
    ).toHaveLength(2);
  });

  it('scopes to a folder, deep or direct', () => {
    const files = [
      file({ origin: fsOrigin(['DCIM']) }),
      file({}),
      file({ origin: { type: 'carved', namedFromMetadata: false } }),
    ];
    const scope = { type: 'folder', path: ['SD', 'DCIM'] } as const;
    expect(applyFilters(files, { ...base, scope })).toHaveLength(2);
    expect(applyFilters(files, { ...base, scope }, false)).toHaveLength(1);
    expect(applyFilters(files, { ...base, scope: { type: 'carved', kind: 'photo' } })).toHaveLength(1);
  });

  it('sorts undated files last and groups by day and origin', () => {
    const a = file({ date: { local: '2025-08-03T09:00:00', source: 'capture' } });
    const b = file({ date: null, origin: { type: 'carved', namedFromMetadata: false } });
    const c = file({ date: { local: '2025-08-14T09:00:00', source: 'capture' } });
    const d = file({
      date: { local: '2025-08-14T08:00:00', source: 'capture' },
      origin: { type: 'carved', namedFromMetadata: true },
    });
    const sorted = sortFiles([a, b, c, d], 'newest');
    expect(sorted.map((f) => f.id)).toEqual([c.id, d.id, a.id, b.id]);
    const groups = groupByDay(sorted);
    expect(groups.map((g) => [g.day, g.files.length])).toEqual([
      ['2025-08-14', 1],
      ['2025-08-14', 1],
      ['2025-08-03', 1],
      [null, 1],
    ]);
  });

  it('builds a folder tree with descendant counts', () => {
    const files = [
      file({}),
      file({}),
      file({ origin: fsOrigin(['DCIM', '101CANON']) }),
      file({ origin: fsOrigin(['MISC']) }),
    ];
    expect(folderTree(files).map((r) => [r.name, r.depth, r.count])).toEqual([
      ['SD', 0, 4],
      ['DCIM', 1, 3],
      ['100CANON', 2, 2],
      ['101CANON', 2, 1],
      ['MISC', 1, 1],
    ]);
    expect(subfolders(files, ['SD']).map((r) => r.name)).toEqual(['DCIM', 'MISC']);
  });

  it('names unlabeled volumes by position', () => {
    const f = file({ origin: fsOrigin(['DCIM'], null) });
    expect(folderTree([f])[0]?.name).toBe(t('results.volume', { n: 1 }));
  });
});
