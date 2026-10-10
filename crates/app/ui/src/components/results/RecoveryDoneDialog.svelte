<script lang="ts">
  import type { RecoveryReport } from '../../lib/bindings/RecoveryReport';
  import { t } from '../../lib/i18n';
  import Dialog from '../Dialog.svelte';

  let { report, onclose }: { report: RecoveryReport | null; onclose: () => void } = $props();

  const title = $derived(
    report === null
      ? ''
      : t('recoveryDone.title', { count: report.recovered }) +
          (report.partiallyDamaged > 0 ? t('recoveryDone.partial', { count: report.partiallyDamaged }) : ''),
  );
</script>

<Dialog open={report !== null} {title} {onclose}>
  {#if report}
    <p>{t('recoveryDone.byDate')}</p>
    {#if report.failed > 0}<p>{t('recoveryDone.failed', { count: report.failed })}</p>{/if}
    <p class="folder">{t('recoveryDone.folder', { folder: report.folder })}</p>
  {/if}
  {#snippet actions()}
    <button type="button" class="btn-primary" onclick={onclose}>{t('common.ok')}</button>
  {/snippet}
</Dialog>

<style>
  p { margin: 0 0 6px; }
  .folder { overflow-wrap: anywhere; color: var(--text-3); }
</style>
