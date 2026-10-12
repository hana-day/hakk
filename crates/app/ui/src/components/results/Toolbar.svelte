<script lang="ts">
  // Search, filters, order and view mode above the results.
  import { t } from '../../lib/i18n';
  import {
    isFiltered,
    noFilters,
    type DeletedFilter,
    type Filters,
    type SizeFilter,
    type SortOrder,
    type ViewMode,
  } from '../../lib/results';
  import Icon from '../Icon.svelte';

  let {
    filters = $bindable(),
    mode = $bindable(),
    sort = $bindable(),
    years,
  }: { filters: Filters; mode: ViewMode; sort: SortOrder; years: string[] } = $props();

  const modes: ViewMode[] = ['list', 'thumbnails'];
  const deletedOptions: DeletedFilter[] = ['all', 'deleted', 'existing'];
  const sizeOptions: SizeFilter[] = ['all', 'small', 'medium', 'large'];
  const sorts: SortOrder[] = ['newest', 'oldest', 'name', 'size'];

  function reset() {
    filters = { ...noFilters, showHidden: filters.showHidden };
  }
</script>

<div class="toolbar">
  <div class="top">
    <label class="search">
      <Icon name="search" size={16} />
      <input
        type="search"
        placeholder={t('filter.search')}
        aria-label={t('filter.searchLabel')}
        bind:value={filters.query}
      />
    </label>
    <div role="group" aria-label={t('view.label')} class="segments">
      {#each modes as m (m)}
        <button type="button" aria-pressed={mode === m} class:on={mode === m} onclick={() => (mode = m)}>
          {t(`view.${m}`)}
        </button>
      {/each}
    </div>
  </div>
  <div class="filters">
    <select class="chip" class:on={filters.deleted !== 'all'} aria-label={t('filter.deletedLabel')} bind:value={filters.deleted}>
      {#each deletedOptions as d (d)}<option value={d}>{t(`filter.deleted.${d}`)}</option>{/each}
    </select>
    <select class="chip" class:on={filters.size !== 'all'} aria-label={t('filter.sizeLabel')} bind:value={filters.size}>
      {#each sizeOptions as s (s)}<option value={s}>{t(`filter.size.${s}`)}</option>{/each}
    </select>
    <select class="chip" class:on={filters.year !== null} aria-label={t('filter.yearLabel')} bind:value={filters.year}>
      <option value={null}>{t('filter.yearAll')}</option>
      {#each years as y (y)}<option value={y}>{t('filter.year', { year: y })}</option>{/each}
    </select>
    <button
      type="button"
      class="chip"
      class:on={filters.recoverableOnly}
      aria-pressed={filters.recoverableOnly}
      onclick={() => (filters.recoverableOnly = !filters.recoverableOnly)}
    >
      {t('filter.recoverableOnly')}
    </button>
    <select class="chip" aria-label={t('filter.sortLabel')} bind:value={sort}>
      {#each sorts as s (s)}<option value={s}>{t(`filter.sort.${s}`)}</option>{/each}
    </select>
    {#if isFiltered(filters)}
      <button type="button" class="reset" onclick={reset}>{t('filter.reset')}</button>
    {/if}
  </div>
</div>

<style>
  .toolbar { display: flex; flex-direction: column; gap: 10px; }
  .top { display: flex; align-items: center; gap: 12px; flex-wrap: wrap; }
  .search {
    flex: 1 1 260px;
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 40px;
    padding: 0 12px;
    border-radius: 10px;
    border: 1px solid var(--line-strong);
    background: var(--surface);
    color: var(--text-3);
  }
  .search input { flex: 1 1 auto; min-width: 0; border: 0; outline: none; background: transparent; font-size: 14px; color: var(--text); }
  .segments { display: inline-flex; padding: 3px; border-radius: 10px; background: var(--track); }
  .segments button {
    min-height: 34px;
    padding: 0 14px;
    border: 0;
    border-radius: 8px;
    background: transparent;
    font-size: 14px;
    cursor: pointer;
  }
  .segments button.on { background: var(--surface); font-weight: 700; box-shadow: 0 1px 2px rgba(21, 24, 29, 0.12); }
  .filters { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; }
  .chip {
    min-height: 34px;
    padding: 0 12px;
    border-radius: 17px;
    border: 1px solid var(--line-strong);
    background: var(--surface);
    font-size: 13px;
    cursor: pointer;
  }
  select.chip { padding-right: 8px; }
  .chip.on { border-color: var(--brand); background: var(--brand-soft); color: var(--brand-text); }
  .reset { margin-left: auto; border: 0; background: none; color: var(--brand); font-size: 13px; cursor: pointer; }
</style>
