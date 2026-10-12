<script lang="ts">
  import type { FoundFile } from '../../lib/bindings/FoundFile';
  import type { ScanId } from '../../lib/bindings/ScanId';
  import Sentinel from '../Sentinel.svelte';
  import Tile from './Tile.svelte';

  let {
    scanId,
    files,
    selection,
    previewId,
    ontoggle,
    onpreview,
  }: {
    scanId: ScanId;
    files: FoundFile[];
    selection: ReadonlySet<number>;
    previewId: number | null;
    ontoggle: (id: number) => void;
    onpreview: (id: number) => void;
  } = $props();

  /** Tiles rendered per page; more are added as the user scrolls. */
  const PAGE = 240;
  let limit = $state(PAGE);
  const visible = $derived(files.slice(0, limit));
</script>

<div class="grid">
  {#each visible as file (file.id)}
    <Tile
      {scanId}
      {file}
      selected={selection.has(file.id)}
      previewed={previewId === file.id}
      {ontoggle}
      {onpreview}
    />
  {/each}
</div>
{#if visible.length < files.length}
  <Sentinel onvisible={() => (limit += PAGE)} />
{/if}

<style>
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(150px, 1fr)); gap: 14px; }
</style>
