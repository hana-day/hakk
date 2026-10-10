<script lang="ts">
  import type { Destination } from '../lib/bindings/Destination';
  import type { RepairMethod } from '../lib/bindings/RepairMethod';
  import { driveTitle, errorText } from '../lib/display';
  import { t } from '../lib/i18n';
  import {
    applyFilters,
    countCarved,
    passesFilters,
    sortFiles,
    subfolders,
    type KindFilter,
    type Scope,
    type SortOrder,
    type ViewMode,
  } from '../lib/results';
  import type { ScanSession } from '../lib/session.svelte';
  import { showToast } from '../lib/toast.svelte';
  import Dialog from '../components/Dialog.svelte';
  import Icon from '../components/Icon.svelte';
  import Logo from '../components/Logo.svelte';
  import Msg from '../components/Msg.svelte';
  import Sentinel from '../components/Sentinel.svelte';
  import DestinationDialog from '../components/results/DestinationDialog.svelte';
  import FolderView from '../components/results/FolderView.svelte';
  import ListView from '../components/results/ListView.svelte';
  import ProgressPanel from '../components/results/ProgressPanel.svelte';
  import RecoveryBanner from '../components/results/RecoveryBanner.svelte';
  import RecoveryDoneDialog from '../components/results/RecoveryDoneDialog.svelte';
  import SelectionBar from '../components/results/SelectionBar.svelte';
  import Sidebar from '../components/results/Sidebar.svelte';
  import SummaryPanel from '../components/results/SummaryPanel.svelte';
  import ThumbnailView from '../components/results/ThumbnailView.svelte';
  import Toolbar from '../components/results/Toolbar.svelte';

  let { session, onleave }: { session: ScanSession; onleave: () => Promise<void> } = $props();

  /** Items rendered per page; more are added as the user scrolls. */
  const PAGE = 240;

  let scope = $state<Scope>({ type: 'all' });
  let mode = $state<ViewMode>('thumbnails');
  let kind = $state<KindFilter>('all');
  let recoverableOnly = $state(false);
  let sort = $state<SortOrder>('newest');
  let showHidden = $state(false);
  let limit = $state(PAGE);

  let choosingDestination = $state(false);
  let confirming = $state<'cancel' | 'leave' | null>(null);

  // Start from the top whenever what is listed changes.
  $effect(() => {
    void [scope, mode, kind, recoverableOnly, sort, showHidden];
    limit = PAGE;
  });

  const filters = $derived({ scope, kind, recoverableOnly, showHidden });
  const others = $derived({ kind, recoverableOnly, showHidden });
  /** Every found file the current filters would show, for sidebar counts. */
  const countable = $derived(session.allFiles.filter((f) => passesFilters(f, others)));
  const shown = $derived(session.shownFiles.filter((f) => passesFilters(f, others)));
  const matching = $derived(sortFiles(applyFilters(shown, filters, mode !== 'folder'), sort));
  const visible = $derived(matching.slice(0, limit));
  const pendingCount = $derived(
    session.pendingFiles.filter((f) => passesFilters(f, { kind: 'all', recoverableOnly: false, showHidden }))
      .length,
  );
  const hiddenCount = $derived(session.allFiles.filter((f) => f.hidden !== null).length);

  const folderRows = $derived(
    mode !== 'folder' || scope.type === 'carved'
      ? []
      : subfolders(shown, scope.type === 'folder' ? scope.path : []),
  );
  const carvedFolders = $derived(
    mode === 'folder' && scope.type === 'all'
      ? (['photo', 'video'] as const)
          .map((k) => ({ kind: k, count: countCarved(shown, k) }))
          .filter((c) => c.count > 0)
      : [],
  );
  // At the top of the folder view, files sit inside the folders above.
  const listed = $derived(mode === 'folder' && scope.type === 'all' ? [] : visible);

  const breadcrumb = $derived.by(() => {
    const sep = t('results.breadcrumbSeparator');
    switch (scope.type) {
      case 'all':
        return t('sidebar.all');
      case 'folder':
        return scope.path.join(sep);
      case 'carved':
        return t('results.carved') + sep + (scope.kind === 'photo' ? t('sidebar.photos') : t('sidebar.videos'));
    }
  });

  const paused = $derived(session.progress?.state === 'paused');

  async function attempt(action: () => Promise<unknown>) {
    try {
      await action();
    } catch (err) {
      showToast(errorText(err), 'error');
    }
  }

  function toggle(id: number) {
    if (!session.selection.delete(id)) session.selection.add(id);
  }

  function selectAll(select: boolean) {
    for (const f of matching) {
      if (select) session.selection.add(f.id);
      else session.selection.delete(f.id);
    }
  }

  function repair(id: number, method: RepairMethod) {
    if (session.destination === null) {
      choosingDestination = true;
      return;
    }
    void attempt(() => session.repair(id, method));
  }

  function chooseDestination(d: Destination) {
    session.destination = d;
    choosingDestination = false;
  }

  // Report finished repairs.
  $effect(() => {
    const report = session.lastRepair;
    if (report === null) return;
    const name = session.file(report.file)?.name ?? '';
    showToast(
      report.succeeded ? t('repair.done', { name }) : t('repair.failed', { name }),
      report.succeeded ? 'info' : 'error',
    );
    session.lastRepair = null;
  });

  async function confirmed() {
    const what = confirming;
    confirming = null;
    if (what === 'cancel') await attempt(() => session.cancel());
    if (what === 'leave') await attempt(onleave);
  }
</script>

<div class="page">
  <header>
    <div class="left">
      <button type="button" class="home" aria-label={t('header.home')} onclick={() => (confirming = 'leave')}>
        <Logo />
      </button>
      <span class="divider"></span>
      <span class="drive">{driveTitle(session.drive)}</span>
    </div>
    <div class="actions">
      {#if session.scanning}
        {#if paused}
          <button type="button" class="btn-secondary" onclick={() => attempt(() => session.resume())}>
            <Icon name="play" size={14} />{t('header.resume')}
          </button>
        {:else}
          <button
            type="button"
            class="btn-secondary"
            disabled={session.progress?.state === 'pausedForRecovery'}
            onclick={() => attempt(() => session.pause())}
          >
            <Icon name="pause" size={14} stroke={2.4} />{t('header.pause')}
          </button>
        {/if}
        <button type="button" class="btn-quiet" onclick={() => (confirming = 'cancel')}>{t('header.cancel')}</button>
      {:else}
        <button type="button" class="btn-secondary" onclick={() => showToast(t('common.notAvailable'))}>
          {t('header.saveResults')}
        </button>
      {/if}
    </div>
  </header>

  {#if session.summary}
    <SummaryPanel summary={session.summary} />
  {:else if session.progress}
    <ProgressPanel progress={session.progress} />
  {/if}

  {#if session.recovery}
    <RecoveryBanner
      recovery={session.recovery}
      scanPaused={session.progress?.state === 'pausedForRecovery'}
    />
  {/if}

  <div class="body">
    <Sidebar files={countable} {scope} onselect={(s) => (scope = s)} />

    <main>
      <Toolbar bind:mode bind:kind bind:recoverableOnly bind:sort />

      <div class="context">
        <span><Msg key="results.context" params={{ place: breadcrumb, count: matching.length }} mono={['count']} /></span>
      </div>

      {#if pendingCount > 0}
        <button type="button" class="show-new" onclick={() => session.showPending()}>
          <Icon name="up" size={14} stroke={2.4} />
          <Msg key="results.showNew" params={{ count: pendingCount }} />
        </button>
      {/if}

      {#if matching.length === 0 && folderRows.length === 0 && carvedFolders.length === 0}
        <p class="empty">
          {session.scanning && session.allFiles.length === 0 ? t('results.emptyScanning') : t('results.empty')}
        </p>
      {:else if mode === 'folder'}
        <FolderView
          folders={folderRows}
          carved={carvedFolders}
          files={listed}
          selection={session.selection}
          ontoggle={toggle}
          onopen={(s) => (scope = s)}
        />
      {:else if mode === 'thumbnails'}
        <ThumbnailView
          files={visible}
          grouped={sort === 'newest' || sort === 'oldest'}
          selection={session.selection}
          ontoggle={toggle}
        />
      {:else}
        <ListView
          files={visible}
          allIds={matching.map((f) => f.id)}
          selection={session.selection}
          repairing={session.repairing}
          ontoggle={toggle}
          onselectall={selectAll}
          onrepair={repair}
        />
      {/if}

      {#if visible.length < matching.length}
        <Sentinel onvisible={() => (limit += PAGE)} />
      {/if}

      {#if hiddenCount > 0}
        <p class="hidden-note">
          {showHidden ? t('results.hiddenShown') : t('results.hiddenNote', { count: hiddenCount })}
          <button type="button" class="btn-link" onclick={() => (showHidden = !showHidden)}>
            {showHidden ? t('results.hideHidden') : t('results.showHidden')}
          </button>
        </p>
      {/if}
    </main>
  </div>

  <SelectionBar
    count={session.selection.size}
    size={session.selectedSize}
    destination={session.destination}
    recovering={session.recovery !== null}
    onchange={() => (choosingDestination = true)}
    onrecover={() => attempt(() => session.recover())}
  />
</div>

<DestinationDialog
  open={choosingDestination}
  destinations={session.destinations}
  current={session.destination}
  onchoose={chooseDestination}
  onclose={() => (choosingDestination = false)}
/>

<RecoveryDoneDialog report={session.recoveryReport} onclose={() => (session.recoveryReport = null)} />

<Dialog
  open={confirming !== null}
  title={confirming === 'cancel' ? t('confirm.cancelScan.title') : t('confirm.leave.title')}
  onclose={() => (confirming = null)}
>
  <p>
    {#if confirming === 'cancel'}{t('confirm.cancelScan.body')}
    {:else if session.scanning}{t('confirm.leave.bodyScanning')}
    {:else}{t('confirm.leave.bodyDone')}{/if}
  </p>
  {#snippet actions()}
    <button type="button" class="btn-quiet" onclick={() => (confirming = null)}>
      {confirming === 'cancel' ? t('confirm.cancelScan.keep') : t('common.cancel')}
    </button>
    <button type="button" class="btn-primary" onclick={confirmed}>
      {confirming === 'cancel' ? t('confirm.cancelScan.confirm') : t('confirm.leave.confirm')}
    </button>
  {/snippet}
</Dialog>

<style>
  /* Header, progress and footer stay put; only the sidebar and the results scroll. */
  .page { height: 100vh; display: flex; flex-direction: column; }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
    gap: 12px;
    padding: 12px 24px;
    background: var(--surface);
    border-bottom: 1px solid var(--line);
  }
  .left { display: flex; align-items: center; flex-wrap: wrap; gap: 10px 16px; }
  .home {
    display: flex;
    align-items: center;
    gap: 10px;
    min-height: 44px;
    padding: 0;
    border: 0;
    background: none;
    cursor: pointer;
  }
  .divider { width: 1px; height: 20px; background: var(--line); }
  .drive { font-size: 14px; color: var(--text-2); }
  .actions { display: flex; align-items: center; gap: 8px; }
  .body { flex: 1 1 auto; min-height: 0; display: flex; align-items: stretch; }
  main {
    flex: 1 1 auto;
    min-width: 0;
    overflow-y: auto;
    padding: 18px 24px 24px;
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .context { font-size: 14px; color: var(--text-3); }
  .show-new {
    position: sticky;
    top: 12px;
    z-index: 4;
    align-self: center;
    display: inline-flex;
    align-items: center;
    gap: 8px;
    min-height: 40px;
    padding: 0 18px;
    border-radius: 20px;
    border: 0;
    background: var(--text);
    color: #fff;
    font-size: 14px;
    cursor: pointer;
    box-shadow: 0 4px 12px rgba(21, 24, 29, 0.2);
  }
  .empty { margin: 40px 0; text-align: center; color: var(--text-3); }
  .hidden-note { margin: 0; font-size: 13px; color: var(--text-3); text-align: center; }
  @media (max-width: 860px) {
    .body { flex-direction: column; }
    .body :global(nav) { flex: none; max-height: 30vh; border-right: 0; border-bottom: 1px solid var(--line); }
  }
</style>
