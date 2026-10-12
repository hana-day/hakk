<script lang="ts">
  // A found file's picture: asked for when it scrolls into view, an icon until then (or if
  // no picture can be made).
  import type { FoundFile } from '../../lib/bindings/FoundFile';
  import type { ScanId } from '../../lib/bindings/ScanId';
  import { cancel, request, thumbnail } from '../../lib/thumbs.svelte';
  import Icon from '../Icon.svelte';

  let {
    scanId,
    file,
    size,
    fit = 'cover',
  }: { scanId: ScanId; file: FoundFile; size: number; fit?: 'cover' | 'contain' } = $props();

  let element = $state<HTMLElement | null>(null);
  const url = $derived(thumbnail(scanId, file.id, size));

  $effect(() => {
    const el = element;
    if (el === null) return;
    const id = file.id;
    const observer = new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          if (e.isIntersecting) request(scanId, id, size);
          else cancel(scanId, id, size);
        }
      },
      { rootMargin: '200px' },
    );
    observer.observe(el);
    return () => {
      observer.disconnect();
      cancel(scanId, id, size);
    };
  });
</script>

<span class="thumb" bind:this={element}>
  {#if url}
    <img src={url} alt="" style:object-fit={fit} />
  {:else}
    <span class="icon" class:waiting={url === undefined}>
      <Icon name={file.kind} size={Math.min(28, Math.max(14, size / 6))} stroke={1.6} />
    </span>
  {/if}
</span>

<style>
  .thumb {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 100%;
    height: 100%;
    overflow: hidden;
    background: var(--neutral-soft);
  }
  img { width: 100%; height: 100%; display: block; }
  .icon { color: var(--text-3); display: flex; }
  .icon.waiting { opacity: 0.5; }
</style>
