<script lang="ts">
  import type { DiskMap } from '../../lib/bindings/DiskMap';
  import { t } from '../../lib/i18n';

  let { map, percent, errors }: { map: DiskMap; percent: number; errors: number } = $props();

  // Only scans that read part of the disk have skipped cells; show the legend entry then.
  const hasSkipped = $derived(map.cells.includes('skipped'));
</script>

<div class="map-wrap">
  <div class="map" role="img" aria-label={t('diskMap.label', { percent })}>
    {#each map.cells as cell, i (i)}
      <span class="cell {cell}" class:current={i === map.current}></span>
    {/each}
  </div>
  <div class="legend">
    {#if hasSkipped}<span><i class="cell skipped"></i>{t('diskMap.skipped')}</span>{/if}
    <span><i class="cell unread"></i>{t('diskMap.unread')}</span>
    <span><i class="cell read"></i>{t('diskMap.read')}</span>
    <span><i class="cell found"></i>{t('diskMap.found')}</span>
    <span><i class="cell current"></i>{t('diskMap.current')}</span>
    <span><i class="cell error"></i>{t('diskMap.error', { count: errors })}</span>
  </div>
</div>

<style>
  .map-wrap { flex: 1 1 520px; max-width: 720px; min-width: 0; display: flex; flex-direction: column; gap: 8px; }
  .map { display: grid; grid-template-columns: repeat(48, minmax(0, 1fr)); gap: 2px; }
  .map .cell { display: block; aspect-ratio: 1; }
  .cell { border-radius: 2px; background: var(--track); }
  .cell.skipped { background: transparent; box-shadow: inset 0 0 0 1px var(--track); }
  .cell.read { background: var(--map-read); }
  .cell.found { background: var(--brand); }
  .cell.error { background: var(--error); }
  .cell.current { background: var(--text); }
  .legend { display: flex; flex-wrap: wrap; gap: 6px 16px; font-size: 12px; color: var(--text-3); }
  .legend span { display: inline-flex; align-items: center; gap: 6px; }
  .legend i { display: inline-block; width: 10px; height: 10px; }
</style>
