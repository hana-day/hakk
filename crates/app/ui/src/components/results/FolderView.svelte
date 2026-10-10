<script lang="ts">
  import type { FoundFile } from '../../lib/bindings/FoundFile';
  import { formatCount } from '../../lib/format';
  import { t } from '../../lib/i18n';
  import type { FolderRow, Scope } from '../../lib/results';
  import Icon from '../Icon.svelte';
  import Tile from './Tile.svelte';

  let {
    folders,
    carved,
    files,
    selection,
    ontoggle,
    onopen,
  }: {
    folders: FolderRow[];
    /** Virtual folders for carved files, shown at the top level. */
    carved: { kind: 'photo' | 'video'; count: number }[];
    files: FoundFile[];
    selection: ReadonlySet<number>;
    ontoggle: (id: number) => void;
    onopen: (scope: Scope) => void;
  } = $props();
</script>

<div class="grid">
  {#each folders as f (f.path.join('/'))}
    <button
      type="button"
      class="folder"
      aria-label={t('results.openFolder', { name: f.name })}
      onclick={() => onopen({ type: 'folder', path: f.path })}
    >
      <Icon name="folder" size={40} stroke={1.4} />
      <span class="name">{f.name}</span>
      <span class="num count">{t('results.count', { count: formatCount(f.count) })}</span>
    </button>
  {/each}
  {#each carved as c (c.kind)}
    {@const name = c.kind === 'photo' ? t('sidebar.photos') : t('sidebar.videos')}
    <button
      type="button"
      class="folder"
      aria-label={t('results.openFolder', { name })}
      onclick={() => onopen({ type: 'carved', kind: c.kind })}
    >
      <Icon name={c.kind} size={40} stroke={1.4} />
      <span class="name">{name}</span>
      <span class="sub">{t('results.carved')}</span>
      <span class="num count">{t('results.count', { count: formatCount(c.count) })}</span>
    </button>
  {/each}
  {#each files as file (file.id)}
    <Tile {file} selected={selection.has(file.id)} {ontoggle} />
  {/each}
</div>

<style>
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(150px, 1fr)); gap: 14px; }
  .folder {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 6px;
    min-height: 180px;
    padding: 16px 10px;
    margin: 1px;
    border-radius: 12px;
    border: 1px solid var(--line);
    background: var(--surface);
    color: var(--text-3);
    cursor: pointer;
  }
  .folder:hover { border-color: var(--brand); }
  .name { font-size: 14px; font-weight: 700; color: var(--text); }
  .sub, .count { font-size: 12px; }
</style>
