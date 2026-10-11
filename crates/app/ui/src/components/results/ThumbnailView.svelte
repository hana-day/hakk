<script lang="ts">
  import type { FoundFile } from '../../lib/bindings/FoundFile';
  import { formatLongDate } from '../../lib/format';
  import { t } from '../../lib/i18n';
  import { groupByDay, type Group } from '../../lib/results';
  import Tile from './Tile.svelte';

  let {
    files,
    grouped,
    selection,
    ontoggle,
  }: {
    files: FoundFile[];
    /** Group by capture day (when sorted by date). */
    grouped: boolean;
    selection: ReadonlySet<number>;
    ontoggle: (id: number) => void;
  } = $props();

  const groups = $derived(grouped ? groupByDay(files) : null);

  function label(g: Group): string {
    return g.day === null ? t('results.unknownDate') : formatLongDate(g.day);
  }

  function sub(g: Group): string {
    if (g.source.type === 'folder') {
      // The volume is on the drive the user picked; show the folders below it.
      return g.source.folders.slice(1).join(t('results.folderSeparator')) || (g.source.folders[0] ?? '');
    }
    return g.source.namedFromMetadata ? t('results.carvedNamed') : t('results.carved');
  }
</script>

{#if groups}
  {#each groups as g (g.key)}
    <section>
      <div class="head">
        <h2>{label(g)}</h2>
        <span class="sub">{sub(g)}</span>
      </div>
      <div class="grid">
        {#each g.files as file (file.id)}
          <Tile {file} selected={selection.has(file.id)} {ontoggle} />
        {/each}
      </div>
    </section>
  {/each}
{:else}
  <div class="grid">
    {#each files as file (file.id)}
      <Tile {file} selected={selection.has(file.id)} {ontoggle} />
    {/each}
  </div>
{/if}

<style>
  section { display: flex; flex-direction: column; gap: 12px; }
  .head { display: flex; align-items: baseline; flex-wrap: wrap; gap: 4px 12px; }
  h2 { margin: 0; font-size: 16px; font-weight: 700; }
  .sub { font-size: 13px; color: var(--text-3); }
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(150px, 1fr)); gap: 14px; }
</style>
