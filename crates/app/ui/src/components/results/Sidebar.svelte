<script lang="ts">
  import type { FoundFile } from '../../lib/bindings/FoundFile';
  import { formatCount } from '../../lib/format';
  import { t } from '../../lib/i18n';
  import { countCarved, folderTree, sameScope, type Scope } from '../../lib/results';
  import Icon from '../Icon.svelte';

  let {
    files,
    scope,
    onselect,
  }: { files: FoundFile[]; scope: Scope; onselect: (scope: Scope) => void } = $props();

  const tree = $derived(folderTree(files));
  const photos = $derived(countCarved(files, 'photo'));
  const videos = $derived(countCarved(files, 'video'));
</script>

<nav aria-label={t('sidebar.label')}>
  <button
    type="button"
    class="row"
    class:on={scope.type === 'all'}
    aria-current={scope.type === 'all'}
    onclick={() => onselect({ type: 'all' })}
  >
    <span>{t('sidebar.all')}</span><span class="num count">{formatCount(files.length)}</span>
  </button>

  {#if tree.length > 0}
    <span class="heading">{t('sidebar.withFolders')}</span>
    {#each tree as row (row.path.join('/'))}
      {@const target = { type: 'folder', path: row.path } as const}
      {@const on = sameScope(scope, target)}
      <button
        type="button"
        class="row"
        class:on
        aria-current={on}
        style:padding-left={`${12 + row.depth * 16}px`}
        onclick={() => onselect(target)}
      >
        <span class="label"><Icon name="folder" stroke={1.8} /><span class="name">{row.name}</span></span>
        <span class="num count">{formatCount(row.count)}</span>
      </button>
    {/each}
  {/if}

  <span class="heading">{t('sidebar.withoutNames')}</span>
  {#each [['photo', photos], ['video', videos]] as const as [kind, count] (kind)}
    {@const target = { type: 'carved', kind } as const}
    {@const on = sameScope(scope, target)}
    <button type="button" class="row" class:on aria-current={on} onclick={() => onselect(target)}>
      <span class="label">
        <Icon name={kind} stroke={1.8} />{kind === 'photo' ? t('sidebar.photos') : t('sidebar.videos')}
      </span>
      <span class="num count">{formatCount(count)}</span>
    </button>
  {/each}
</nav>

<style>
  nav {
    flex: 0 0 260px;
    padding: 18px 12px;
    background: var(--surface);
    border-right: 1px solid var(--line);
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 14px;
    overflow-y: auto;
  }
  .row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 8px;
    min-height: 40px;
    padding: 0 12px;
    border: 0;
    border-radius: 8px;
    background: transparent;
    cursor: pointer;
    text-align: left;
    flex: none;
  }
  .row:hover { background: var(--bg); }
  .row.on { background: var(--brand-soft); color: var(--brand-text); font-weight: 700; }
  .label { display: inline-flex; align-items: center; gap: 8px; min-width: 0; color: inherit; }
  .label :global(svg) { color: var(--text-3); }
  .on .label :global(svg) { color: currentColor; }
  .name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .count { color: var(--text-3); }
  .on .count { color: inherit; }
  .heading { margin: 14px 12px 4px; font-size: 12px; font-weight: 700; color: var(--text-3); }
</style>
