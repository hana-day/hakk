<script lang="ts">
  import type { ScanSummary } from '../../lib/bindings/ScanSummary';
  import { formatCount, formatElapsed } from '../../lib/format';
  import { t } from '../../lib/i18n';
  import Icon from '../Icon.svelte';

  let { summary }: { summary: ScanSummary } = $props();
</script>

<section aria-label={t('summary.label')} class="panel">
  <div class="head">
    <span class="mark"><Icon name="check" size={22} stroke={2.6} /></span>
    <div class="text">
      <span class="title">{summary.cancelled ? t('summary.cancelled') : t('summary.finished')}</span>
      <span class="sub">{t('summary.found', { photos: summary.photos, videos: summary.videos })}</span>
    </div>
  </div>
  <div class="stats">
    <div class="stat">
      <span class="stat-label">{t('summary.recoverable')}</span>
      <span class="num value">{formatCount(summary.recoverable)}</span>
    </div>
    <div class="stat">
      <span class="stat-label">{t('summary.partiallyDamaged')}</span>
      <span class="num value">{formatCount(summary.partiallyDamaged)}</span>
    </div>
    <div class="stat">
      <span class="stat-label">{t('summary.elapsed')}</span>
      <span class="num value">{formatElapsed(summary.elapsedSeconds)}</span>
    </div>
    <div class="stat">
      <span class="stat-label">{t('summary.readErrors')}</span>
      <span class="num value">{t('summary.readErrorsValue', { count: summary.readErrors })}</span>
    </div>
  </div>
</section>

<style>
  .panel {
    background: var(--surface);
    border-bottom: 1px solid var(--line);
    padding: 20px 24px;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 16px 32px;
  }
  .head { display: flex; align-items: center; gap: 14px; }
  .mark {
    flex: none;
    width: 44px;
    height: 44px;
    border-radius: 50%;
    background: var(--brand-soft);
    color: var(--brand);
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .text { display: flex; flex-direction: column; gap: 4px; }
  .title { font-size: 20px; font-weight: 700; }
  .sub { font-size: 14px; color: var(--text-3); }
  .stats { display: flex; flex-wrap: wrap; gap: 10px; }
  .value { font-size: 18px; }
</style>
