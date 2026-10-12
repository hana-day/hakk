<script lang="ts">
  // File types: all results, photos and videos, and under each its formats.
  import type { FileKind } from '../../lib/bindings/FileKind';
  import { formatCount } from '../../lib/format';
  import { t } from '../../lib/i18n';
  import { formatLabel, type TypeCounts, type TypeScope } from '../../lib/results';
  import Icon from '../Icon.svelte';

  let {
    counts,
    type,
    onselect,
  }: { counts: TypeCounts; type: TypeScope; onselect: (type: TypeScope) => void } = $props();

  const kinds: { kind: FileKind; label: string }[] = [
    { kind: 'photo', label: t('sidebar.photos') },
    { kind: 'video', label: t('sidebar.videos') },
  ];

  function on(kind: TypeScope['kind'], format: string | null): boolean {
    return type.kind === kind && type.format === format;
  }
</script>

<nav aria-label={t('sidebar.label')}>
  <button
    type="button"
    class="row"
    class:on={on('all', null)}
    aria-current={on('all', null)}
    onclick={() => onselect({ kind: 'all', format: null })}
  >
    <span class="label"><Icon name="search" stroke={1.8} />{t('sidebar.all')}</span>
    <span class="num count">{formatCount(counts.all)}</span>
  </button>

  {#each kinds as k (k.kind)}
    <button
      type="button"
      class="row"
      class:on={on(k.kind, null)}
      aria-current={on(k.kind, null)}
      onclick={() => onselect({ kind: k.kind, format: null })}
    >
      <span class="label"><Icon name={k.kind} stroke={1.8} />{k.label}</span>
      <span class="num count">{formatCount(counts[k.kind])}</span>
    </button>
    {#each counts.formats[k.kind] as f (f.format)}
      <button
        type="button"
        class="row sub"
        class:on={on(k.kind, f.format)}
        aria-current={on(k.kind, f.format)}
        onclick={() => onselect({ kind: k.kind, format: f.format })}
      >
        <span class="label">{formatLabel(f.format)}</span>
        <span class="num count">{formatCount(f.count)}</span>
      </button>
    {/each}
  {/each}
</nav>

<style>
  nav {
    flex: 0 0 220px;
    padding: 14px 10px;
    background: var(--surface);
    border-right: 1px solid var(--line);
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: 14px;
    overflow-y: auto;
  }
  .row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 8px;
    min-height: 38px;
    padding: 0 12px;
    border: 0;
    border-radius: 8px;
    background: transparent;
    cursor: pointer;
    text-align: left;
    flex: none;
  }
  .row.sub { min-height: 32px; padding-left: 40px; font-size: 13px; }
  .row:hover { background: var(--bg); }
  .row.on { background: var(--brand-soft); color: var(--brand-text); font-weight: 700; }
  .label { display: inline-flex; align-items: center; gap: 8px; min-width: 0; color: inherit; }
  .label :global(svg) { color: var(--text-3); }
  .on .label :global(svg) { color: currentColor; }
  .count { color: var(--text-3); font-size: 12px; }
  .on .count { color: inherit; }
</style>
