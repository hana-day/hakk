<script lang="ts">
  import type { Destination } from '../../lib/bindings/Destination';
  import { formatBytes } from '../../lib/format';
  import { t } from '../../lib/i18n';
  import Icon from '../Icon.svelte';
  import Msg from '../Msg.svelte';

  let {
    count,
    size,
    destination,
    recovering,
    onchange,
    onrecover,
  }: {
    count: number;
    size: number;
    destination: Destination | null;
    recovering: boolean;
    onchange: () => void;
    onrecover: () => void;
  } = $props();
</script>

<footer>
  <span class="selected">
    <strong><Msg key="footer.selected" params={{ count }} /></strong>{t('footer.selectedSuffix')}<span
      class="num size">{formatBytes(size)}</span
    >
  </span>
  <div class="right">
    <span class="dest">
      {destination
        ? t('footer.destination', {
            place: t('place', { kind: t(`driveKind.${destination.kind}`), name: destination.name }),
          })
        : t('footer.destinationNone')}
      <button type="button" class="btn-link" onclick={onchange}>{t('footer.change')}</button>
    </span>
    <button
      type="button"
      class="btn-primary"
      disabled={count === 0 || destination === null || recovering}
      onclick={onrecover}
    >
      <Icon name="download" size={18} stroke={2.2} />
      {recovering ? t('footer.recovering') : t('footer.recover')}
    </button>
  </div>
</footer>

<style>
  footer {
    flex: none;
    display: flex;
    justify-content: space-between;
    align-items: center;
    flex-wrap: wrap;
    gap: 12px 20px;
    padding: 14px 24px;
    background: var(--surface);
    border-top: 1px solid var(--line);
    box-shadow: 0 -4px 12px rgba(21, 24, 29, 0.06);
    z-index: 5;
  }
  .selected { font-size: 15px; }
  .size { color: var(--text-3); }
  .right { display: flex; align-items: center; flex-wrap: wrap; gap: 12px 16px; }
  .dest { display: inline-flex; align-items: center; gap: 8px; font-size: 14px; color: var(--text-2); }
</style>
