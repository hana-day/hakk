<script lang="ts">
  // The finished scan on one line.
  import type { ScanSummary } from '../../lib/bindings/ScanSummary';
  import { formatCount, formatElapsed } from '../../lib/format';
  import { t } from '../../lib/i18n';
  import Icon from '../Icon.svelte';

  let { summary }: { summary: ScanSummary } = $props();
</script>

<section aria-label={t('summary.label')} class="panel">
  <span class="mark"><Icon name="check" size={14} stroke={3} /></span>
  <span class="title">{summary.cancelled ? t('summary.cancelled') : t('summary.finished')}</span>
  <span class="stats">
    <span>{t('summary.recoverable')} <span class="num">{formatCount(summary.recoverable)}</span></span>
    <span>{t('summary.partiallyDamaged')} <span class="num">{formatCount(summary.partiallyDamaged)}</span></span>
    <span>{t('summary.elapsed')} <span class="num">{formatElapsed(summary.elapsedSeconds)}</span></span>
    <span class:errors={summary.readErrors > 0}>
      {t('summary.readErrors')} <span class="num">{t('summary.readErrorsValue', { count: summary.readErrors })}</span>
    </span>
  </span>
</section>

<style>
  .panel {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px 16px;
    padding: 10px 24px;
    background: var(--surface);
    border-bottom: 1px solid var(--line);
    font-size: 13px;
  }
  .mark {
    flex: none;
    width: 22px;
    height: 22px;
    border-radius: 50%;
    background: var(--brand-soft);
    color: var(--brand);
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .title { font-size: 14px; font-weight: 700; }
  .stats { display: inline-flex; flex-wrap: wrap; gap: 4px 18px; color: var(--text-2); }
  .num { font-weight: 700; color: var(--text); }
  .errors .num { color: var(--danger, #b2402b); }
</style>
