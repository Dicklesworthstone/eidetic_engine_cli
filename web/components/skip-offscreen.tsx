"use client";

import { useEffect } from "react";

/** How long the window's width must hold still before the sections are measured again. */
const SETTLE_MS = 250;

/**
 * Ends every animation in a section that is being skipped, as it would have ended: one skipped part way through never
 * finishes, and the page goes on drawing frames for it.
 */
function finishAnimations(section: Element): void {
  for (const animation of section.getAnimations({ subtree: true })) {
    if (animation.effect?.getComputedTiming().endTime !== Number.POSITIVE_INFINITY) animation.finish();
  }
}

/**
 * Lets the browser skip the style, layout and paint of every section that is far off screen (`.skip-offscreen` in
 * styles/site.tailwind). Each section first keeps the height it was laid out at, so nothing on the page moves: not the
 * scrollbar, the reading progress or an anchor's landing. Wherever the browser cannot do it, the page is drawn whole.
 *
 * A page opened at its top starts with those sections skipped at a guessed height (`.first-view`): as soon as it is idle
 * this lays them out one at a time, so no frame has the whole page to do, and measures them once it has loaded.
 */
export function SkipOffscreen() {
  useEffect(() => {
    const root = document.documentElement;
    if (typeof CSS === "undefined" || !CSS.supports("content-visibility", "auto")) return;
    const sections = Array.from(document.querySelectorAll<HTMLElement>("[data-skip]"));
    let timer = 0;
    let idle = 0;
    let frame = 0;
    let stopped = false;
    let width = window.innerWidth;

    const cancel = () => {
      window.clearTimeout(timer);
      cancelAnimationFrame(frame);
      if (idle && typeof cancelIdleCallback === "function") cancelIdleCallback(idle);
    };
    const whenIdle = (then: () => void, timeout: number) => {
      if (stopped) return;
      if (typeof requestIdleCallback === "function") idle = requestIdleCallback(then, { timeout });
      else timer = window.setTimeout(then, 50);
    };
    const nextFrame = (then: () => void) => {
      frame = requestAnimationFrame(then);
    };

    /** Fixes every section's height, and from here on skips the ones far off screen. */
    const measure = () => {
      if (stopped) return;
      // Read every height before writing any, so this is one layout.
      const heights = sections.map((section) => {
        const style = getComputedStyle(section);
        const edges = ["paddingTop", "paddingBottom", "borderTopWidth", "borderBottomWidth"] as const;
        return section.getBoundingClientRect().height - edges.reduce((sum, edge) => sum + Number.parseFloat(style[edge]), 0);
      });
      sections.forEach((section, index) => {
        section.style.setProperty("--skip-height", `${heights[index]}px`);
        section.style.contentVisibility = "";
      });
      root.classList.remove("first-view");
      root.classList.add("skip-offscreen");
      // No event says so for a section that was skipped before it was laid out.
      for (const section of sections) {
        const { top, bottom } = section.getBoundingClientRect();
        if (bottom < 0 || top > window.innerHeight) finishAnimations(section);
      }
    };
    /** Measures once the figures have fitted (two frames), after the fonts and images, which can change a height. */
    const settle = () => {
      void document.fonts.ready.then(() => nextFrame(() => nextFrame(() => whenIdle(measure, 1500))));
    };
    const whenLoaded = () => {
      if (document.readyState === "complete") settle();
      else window.addEventListener("load", settle, { once: true });
    };
    /** Lays out the sections from `index` on, one to an idle moment. */
    const layOut = (index: number) => {
      if (stopped) return;
      const section = sections[index];
      if (!section || !root.classList.contains("first-view")) {
        whenLoaded();
        return;
      }
      section.style.contentVisibility = "visible";
      nextFrame(() => whenIdle(() => layOut(index + 1), 300));
    };
    whenIdle(() => layOut(0), 1000);

    // A new width lays every section out anew. (A phone's toolbar only changes the height.)
    const onResize = () => {
      if (window.innerWidth === width) return;
      width = window.innerWidth;
      root.classList.remove("skip-offscreen");
      cancel();
      window.removeEventListener("load", settle);
      timer = window.setTimeout(() => layOut(0), SETTLE_MS);
    };
    window.addEventListener("resize", onResize);

    const onStateChange = (event: Event) => {
      const { skipped, target } = event as ContentVisibilityAutoStateChangeEvent;
      if (skipped && target instanceof Element) finishAnimations(target);
    };
    document.addEventListener("contentvisibilityautostatechange", onStateChange);
    return () => {
      stopped = true;
      cancel();
      window.removeEventListener("load", settle);
      window.removeEventListener("resize", onResize);
      document.removeEventListener("contentvisibilityautostatechange", onStateChange);
      root.classList.remove("first-view", "skip-offscreen");
      for (const section of sections) section.style.contentVisibility = "";
    };
  }, []);
  return null;
}
