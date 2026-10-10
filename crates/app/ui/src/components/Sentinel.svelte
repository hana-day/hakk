<script lang="ts">
  // Calls `onvisible` when scrolled near; used to render long lists a page at a time.
  let { onvisible }: { onvisible: () => void } = $props();
  let el: HTMLDivElement | undefined = $state();

  $effect(() => {
    if (!el) return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) onvisible();
      },
      { rootMargin: '800px' },
    );
    observer.observe(el);
    return () => observer.disconnect();
  });
</script>

<div bind:this={el} aria-hidden="true" style="height: 1px"></div>
