<script lang="ts">
  // The results as a table of folders that open and close, with their files.
  import type { ScanId } from '../../lib/bindings/ScanId';
  import { displayName, statusBadge } from '../../lib/display';
  import { formatBytes, formatCount, formatDateTime } from '../../lib/format';
  import { t } from '../../lib/i18n';
  import { filesUnder, flattenTree, formatLabel, type FolderNode } from '../../lib/results';
  import Icon from '../Icon.svelte';
  import Sentinel from '../Sentinel.svelte';
  import Thumb from './Thumb.svelte';

  let {
    scanId,
    roots,
    open,
    selection,
    previewId,
    ontoggle,
    onselectmany,
    onpreview,
  }: {
    scanId: ScanId;
    roots: FolderNode[];
    /** Keys of open folders. */
    open: Set<string>;
    selection: ReadonlySet<number>;
    previewId: number | null;
    ontoggle: (id: number) => void;
    onselectmany: (ids: number[], select: boolean) => void;
    onpreview: (id: number) => void;
  } = $props();

  /** Rows rendered per page; more are added as the user scrolls. */
  const PAGE = 300;
  let limit = $state(PAGE);

  const rows = $derived(flattenTree(roots, open));
  const visible = $derived(rows.slice(0, limit));

  function toggleFolder(node: FolderNode) {
    if (!open.delete(node.key)) open.add(node.key);
  }

  function folderState(node: FolderNode): { checked: boolean; mixed: boolean; ids: number[] } {
    const ids = filesUnder(node);
    const n = ids.filter((id) => selection.has(id)).length;
    return { checked: n > 0 && n === ids.length, mixed: n > 0 && n < ids.length, ids };
  }
</script>

<div class="wrap">
  <table>
    <thead>
      <tr>
        <th scope="col" class="check"></th>
        <th scope="col">{t('list.name')}</th>
        <th scope="col">{t('list.status')}</th>
        <th scope="col">{t('list.date')}</th>
        <th scope="col" class="right">{t('list.size')}</th>
        <th scope="col">{t('list.format')}</th>
      </tr>
    </thead>
    <tbody>
      {#each visible as row (row.type === 'folder' ? `d:${row.node.key}` : `f:${row.file.id}`)}
        {#if row.type === 'folder'}
          {@const node = row.node}
          {@const st = folderState(node)}
          <tr class="folder" onclick={() => toggleFolder(node)}>
            <td class="check">
              <input
                type="checkbox"
                checked={st.checked}
                indeterminate={st.mixed}
                aria-label={t('list.select', { name: node.name })}
                onclick={(e) => e.stopPropagation()}
                onchange={(e) => onselectmany(st.ids, e.currentTarget.checked)}
              />
            </td>
            <td>
              <span class="name" style:padding-left={`${row.depth * 18}px`}>
                <button
                  type="button"
                  class="chevron"
                  class:open={row.open}
                  aria-label={row.open ? t('results.close', { name: node.name }) : t('results.open', { name: node.name })}
                  aria-expanded={row.open}
                  onclick={(e) => {
                    e.stopPropagation();
                    toggleFolder(node);
                  }}
                >
                  <Icon name="chevron" size={14} stroke={2.4} />
                </button>
                <span class="folder-icon" class:carved={node.carved}>
                  <Icon name={node.carved ? 'search' : 'folder'} size={16} stroke={1.8} />
                </span>
                <span class="label">{t('results.folderLabel', { name: node.name, count: formatCount(node.count) })}</span>
              </span>
            </td>
            <td></td>
            <td></td>
            <td class="num right">{formatBytes(node.size)}</td>
            <td class="sub">{t('list.folder')}</td>
          </tr>
        {:else}
          {@const f = row.file}
          {@const on = selection.has(f.id)}
          {@const badge = statusBadge(f)}
          <tr class:on class:previewed={previewId === f.id} onclick={() => onpreview(f.id)}>
            <td class="check">
              <input
                type="checkbox"
                checked={on}
                aria-label={t('list.select', { name: f.name })}
                onclick={(e) => e.stopPropagation()}
                onchange={() => ontoggle(f.id)}
              />
            </td>
            <td>
              <span class="name" style:padding-left={`${row.depth * 18 + 22}px`}>
                <span class="thumb"><Thumb {scanId} file={f} size={96} /></span>
                <span class="label" title={displayName(f)}>{displayName(f)}</span>
              </span>
            </td>
            <td>
              <span class="badges">
                <span class={badge.className}>{badge.text}</span>
                <span class="badge" class:badge-neutral={!f.deleted} class:badge-deleted={f.deleted}>
                  {f.deleted ? t('deleted.deleted') : t('deleted.existing')}
                </span>
              </span>
            </td>
            <td class="date">
              {#if f.date === null}{t('list.unknownDate')}
              {:else if f.date.source === 'fileSystem'}{t('list.fileDate', { date: formatDateTime(f.date.local) })}
              {:else}{formatDateTime(f.date.local)}{/if}
            </td>
            <td class="num right">{formatBytes(f.size)}</td>
            <td class="sub">{formatLabel(f.format)}</td>
          </tr>
        {/if}
      {/each}
    </tbody>
  </table>
  {#if visible.length < rows.length}
    <Sentinel onvisible={() => (limit += PAGE)} />
  {/if}
</div>

<style>
  .wrap { overflow-x: auto; background: var(--surface); border: 1px solid var(--line); border-radius: 12px; }
  table { width: 100%; min-width: 760px; border-collapse: collapse; font-size: 14px; }
  th {
    position: sticky;
    top: 0;
    z-index: 1;
    padding: 10px 12px;
    background: var(--surface);
    border-bottom: 1px solid var(--line);
    text-align: left;
    color: var(--text-3);
    font-size: 12px;
    font-weight: 700;
  }
  td { padding: 6px 12px; border-bottom: 1px solid var(--line-soft); white-space: nowrap; }
  tr { cursor: pointer; }
  tbody tr:hover { background: var(--bg); }
  tr.on, tr.on:hover { background: var(--brand-row); }
  tr.previewed td { box-shadow: inset 0 0 0 9999px rgba(0, 0, 0, 0.03); }
  tr.folder td { font-weight: 600; }
  .check { width: 40px; padding-left: 14px; padding-right: 0; }
  .check input { width: 17px; height: 17px; cursor: pointer; }
  .right { text-align: right; }
  .name { display: flex; align-items: center; gap: 8px; min-width: 0; max-width: 520px; }
  .label { overflow: hidden; text-overflow: ellipsis; }
  .chevron {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    padding: 0;
    border: 0;
    border-radius: 4px;
    background: none;
    color: var(--text-3);
    cursor: pointer;
    transition: transform 0.1s;
  }
  .chevron.open { transform: rotate(90deg); }
  .folder-icon { display: flex; color: #4a90d9; }
  .folder-icon.carved { color: var(--text-3); }
  .thumb { flex: none; width: 40px; height: 30px; border-radius: 4px; overflow: hidden; }
  .badges { display: inline-flex; gap: 6px; }
  .badge { padding: 2px 8px; font-size: 11px; }
  .badge-deleted { background: #fde8e4; color: #b2402b; }
  .date { color: var(--text-2); }
  .sub { color: var(--text-3); font-size: 13px; }
</style>
