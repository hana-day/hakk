<script lang="ts">
  import { t } from '../../lib/i18n';
  import type { KindFilter, SortOrder, ViewMode } from '../../lib/results';

  let {
    mode = $bindable(),
    kind = $bindable(),
    recoverableOnly = $bindable(),
    sort = $bindable(),
  }: { mode: ViewMode; kind: KindFilter; recoverableOnly: boolean; sort: SortOrder } = $props();

  const modes: ViewMode[] = ['folder', 'thumbnails', 'list'];
  const kinds: KindFilter[] = ['all', 'photo', 'video'];
  const sorts: SortOrder[] = ['newest', 'oldest', 'name', 'size'];
</script>

<div class="toolbar">
  <div role="group" aria-label={t('view.label')} class="segments">
    {#each modes as m (m)}
      <button type="button" aria-pressed={mode === m} class:on={mode === m} onclick={() => (mode = m)}>
        {t(`view.${m}`)}
      </button>
    {/each}
  </div>
  <div class="filters">
    <select class="chip" class:on={kind !== 'all'} aria-label={t('filter.kindLabel')} bind:value={kind}>
      {#each kinds as k (k)}<option value={k}>{t(`filter.kind.${k}`)}</option>{/each}
    </select>
    <button
      type="button"
      class="chip"
      class:on={recoverableOnly}
      aria-pressed={recoverableOnly}
      onclick={() => (recoverableOnly = !recoverableOnly)}
    >
      {t('filter.recoverableOnly')}
    </button>
    <select class="chip" aria-label={t('filter.sortLabel')} bind:value={sort}>
      {#each sorts as s (s)}<option value={s}>{t(`filter.sort.${s}`)}</option>{/each}
    </select>
  </div>
</div>

<style>
  .toolbar { display: flex; justify-content: space-between; align-items: center; flex-wrap: wrap; gap: 12px; }
  .segments { display: inline-flex; padding: 3px; border-radius: 10px; background: var(--track); }
  .segments button {
    min-height: 38px;
    padding: 0 14px;
    border: 0;
    border-radius: 8px;
    background: transparent;
    font-size: 14px;
    cursor: pointer;
  }
  .segments button.on { background: var(--surface); font-weight: 700; box-shadow: 0 1px 2px rgba(21, 24, 29, 0.12); }
  .filters { display: flex; flex-wrap: wrap; gap: 8px; }
  .chip {
    min-height: 36px;
    padding: 0 14px;
    border-radius: 18px;
    border: 1px solid var(--line-strong);
    background: var(--surface);
    font-size: 13px;
    cursor: pointer;
  }
  select.chip { padding-right: 10px; }
  .chip.on { border-color: var(--brand); background: var(--brand-soft); color: var(--brand-text); }
</style>
