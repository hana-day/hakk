<script lang="ts">
  import { onMount } from 'svelte';
  import { api, onHelperEvent } from './lib/api';
  import type { Drive } from './lib/bindings/Drive';
  import type { HelperEvent } from './lib/bindings/HelperEvent';
  import { ScanSession } from './lib/session.svelte';
  import Toasts from './components/Toasts.svelte';
  import DriveSelect from './screens/DriveSelect.svelte';
  import Results from './screens/Results.svelte';

  let session = $state<ScanSession | null>(null);
  // Events that arrive while a scan is starting, before its id is known.
  let early: HelperEvent[] | null = null;

  onMount(() => {
    const unlisten = onHelperEvent((event) => {
      if (early !== null) early.push(event);
      else session?.handle(event);
    });
    return () => void unlisten.then((stop) => stop());
  });

  async function start(drive: Drive) {
    early = [];
    try {
      const scanId = await api.startScan(drive.id);
      const next = new ScanSession(drive, scanId);
      for (const event of early) next.handle(event);
      session = next;
    } finally {
      early = null;
    }
    // The destination list is not needed until the user recovers something.
    session.loadDestinations().catch(() => {});
  }

  async function leave() {
    await session?.cancel();
    session = null;
  }
</script>

{#if session}
  <Results {session} onleave={leave} />
{:else}
  <DriveSelect onstart={start} />
{/if}
<Toasts />
