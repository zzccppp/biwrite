<script lang="ts">
  interface Props {
    /** Fraction of the width given to the left pane. */
    ratio: number;
    container: HTMLElement | undefined;
    onchange: (ratio: number) => void;
  }

  let { ratio, container, onchange }: Props = $props();

  const MIN = 0.22;
  const MAX = 0.78;
  let dragging = $state(false);

  function onpointerdown(e: PointerEvent): void {
    if (!container) return;
    dragging = true;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    e.preventDefault();
  }

  function onpointermove(e: PointerEvent): void {
    if (!dragging || !container) return;
    const rect = container.getBoundingClientRect();
    const next = (e.clientX - rect.left) / rect.width;
    onchange(Math.min(MAX, Math.max(MIN, next)));
  }

  function onpointerup(e: PointerEvent): void {
    dragging = false;
    (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
  }

  function onkeydown(e: KeyboardEvent): void {
    const step = e.shiftKey ? 0.1 : 0.02;
    if (e.key === "ArrowLeft") onchange(Math.max(MIN, ratio - step));
    else if (e.key === "ArrowRight") onchange(Math.min(MAX, ratio + step));
    else return;
    e.preventDefault();
  }
</script>

<!-- A focusable separator is the ARIA "window splitter" pattern (keyboard resizable). -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
<div
  class="splitter"
  class:dragging
  role="separator"
  aria-orientation="vertical"
  aria-valuenow={Math.round(ratio * 100)}
  aria-valuemin={MIN * 100}
  aria-valuemax={MAX * 100}
  tabindex="0"
  title="Drag to resize · double-click to reset"
  {onpointerdown}
  {onpointermove}
  {onpointerup}
  {onkeydown}
  ondblclick={() => onchange(0.5)}
></div>

<style>
  .splitter {
    position: relative;
    flex: none;
    width: 9px;
    margin: 0 -4px;
    z-index: 5;
    cursor: col-resize;
    touch-action: none;
  }
  .splitter::before {
    content: "";
    position: absolute;
    left: 4px;
    top: 0;
    bottom: 0;
    width: 1px;
    background: var(--rule-strong);
    transition:
      background-color 150ms var(--ease),
      box-shadow 150ms var(--ease);
  }
  .splitter:hover::before,
  .splitter.dragging::before,
  .splitter:focus-visible::before {
    background: var(--seal);
    box-shadow: 0 0 0 1px var(--seal-wash);
  }
  .splitter:focus-visible {
    outline: none;
  }
</style>
