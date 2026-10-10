<script lang="ts">
  import type { FoundFile } from '../../lib/bindings/FoundFile';
  import type { RepairMethod } from '../../lib/bindings/RepairMethod';
  import { displayName, statusBadge, tone } from '../../lib/display';
  import { formatBytes, formatClock, formatDateTime } from '../../lib/format';
  import { t } from '../../lib/i18n';
  import Icon from '../Icon.svelte';

  let {
    files,
    allIds,
    selection,
    repairing,
    ontoggle,
    onselectall,
    onrepair,
  }: {
    /** Rows rendered so far. */
    files: FoundFile[];
    /** Every row matching the filters, for "select all". */
    allIds: number[];
    selection: ReadonlySet<number>;
    repairing: ReadonlySet<number>;
    ontoggle: (id: number) => void;
    onselectall: (select: boolean) => void;
    onrepair: (id: number, method: RepairMethod) => void;
  } = $props();

  const selectedHere = $derived(allIds.filter((id) => selection.has(id)).length);
  const allSelected = $derived(allIds.length > 0 && selectedHere === allIds.length);
  const someSelected = $derived(selectedHere > 0 && !allSelected);
</script>

<div class="wrap">
  <table>
    <thead>
      <tr>
        <th scope="col" class="check">
          <input
            type="checkbox"
            aria-label={t('list.selectAll')}
            checked={allSelected}
            indeterminate={someSelected}
            onchange={(e) => onselectall(e.currentTarget.checked)}
          />
        </th>
        <th scope="col">{t('list.name')}</th>
        <th scope="col">{t('list.capturedAt')}</th>
        <th scope="col">{t('list.duration')}</th>
        <th scope="col" class="right">{t('list.size')}</th>
        <th scope="col">{t('list.status')}</th>
        <th scope="col" class="last"><span class="visually-hidden">{t('list.actions')}</span></th>
      </tr>
    </thead>
    <tbody>
      {#each files as f (f.id)}
        {@const on = selection.has(f.id)}
        {@const badge = statusBadge(f)}
        <tr class:on onclick={() => ontoggle(f.id)}>
          <td class="check">
            <input
              type="checkbox"
              checked={on}
              aria-label={t('list.select', { name: f.name })}
              onclick={(e) => e.stopPropagation()}
              onchange={() => ontoggle(f.id)}
            />
          </td>
          <td>
            <span class="name">
              <span class="thumb" style:background-color={tone(f.id)}></span>
              <span class="num mono">{displayName(f)}</span>
            </span>
          </td>
          <td class="date">{f.capturedAt === null ? t('list.unknownDate') : formatDateTime(f.capturedAt)}</td>
          <td class="num mono">{f.durationSeconds === null ? t('list.noDuration') : formatClock(f.durationSeconds)}</td>
          <td class="num mono right">{formatBytes(f.size)}</td>
          <td><span class={badge.className}>{badge.text}</span></td>
          <td class="last">
            {#if f.repair !== null}
              {@const method = f.repair}
              {@const busy = repairing.has(f.id)}
              <button
                type="button"
                class="repair"
                disabled={busy}
                onclick={(e) => {
                  e.stopPropagation();
                  onrepair(f.id, method);
                }}
              >
                <Icon name="repair" size={14} stroke={2.2} />
                {busy ? t('repair.running') : t(`repair.${method}`)}
              </button>
            {/if}
          </td>
        </tr>
      {/each}
    </tbody>
  </table>
</div>

<style>
  .wrap { overflow-x: auto; background: var(--surface); border: 1px solid var(--line); border-radius: 12px; }
  table { width: 100%; min-width: 760px; border-collapse: collapse; font-size: 14px; }
  th {
    padding: 12px;
    border-bottom: 1px solid var(--line);
    text-align: left;
    color: var(--text-3);
    font-size: 12px;
    font-weight: 700;
  }
  td { padding: 10px 12px; border-bottom: 1px solid var(--line-soft); }
  tr { cursor: pointer; }
  tbody tr:hover { background: var(--bg); }
  tr.on, tr.on:hover { background: var(--brand-row); }
  .check { width: 44px; padding-left: 16px; padding-right: 0; }
  .check input { width: 18px; height: 18px; cursor: pointer; }
  .last { padding-right: 16px; text-align: right; }
  .right { text-align: right; }
  .name { display: flex; align-items: center; gap: 10px; }
  .thumb { flex: none; width: 48px; height: 32px; border-radius: 4px; }
  .mono { font-size: 13px; }
  .date { color: var(--text-2); }
  .badge { padding: 3px 10px; font-size: 12px; }
  .repair {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    min-height: 36px;
    padding: 0 12px;
    border-radius: 8px;
    border: 1px solid var(--brand);
    background: var(--surface);
    color: var(--brand);
    font-size: 13px;
    font-weight: 700;
    white-space: nowrap;
    cursor: pointer;
  }
  .repair:disabled { opacity: 0.6; cursor: default; }
</style>
