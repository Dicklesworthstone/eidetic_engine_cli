"use client";

import { type RefObject, useEffect, useLayoutEffect, useRef, useState } from "react";

/** Whether the reader asks for less motion; false until the page has hydrated. */
export function useReducedMotion(): boolean {
  const [reduced, setReduced] = useState(false);
  useEffect(() => {
    const query = window.matchMedia("(prefers-reduced-motion: reduce)");
    const update = () => setReduced(query.matches);
    update();
    query.addEventListener("change", update);
    return () => query.removeEventListener("change", update);
  }, []);
  return reduced;
}

/**
 * Whether `ref`'s element is on screen. Animations use it to stop when they
 * scroll away, so a page of diagrams costs nothing while it is read elsewhere.
 * Without IntersectionObserver it reports true.
 */
export function useInView(ref: RefObject<Element | null>, rootMargin = "120px"): boolean {
  const [inView, setInView] = useState(false);
  useEffect(() => {
    const element = ref.current;
    if (!element) return;
    if (typeof IntersectionObserver === "undefined") {
      setInView(true);
      return;
    }
    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) setInView(entry.isIntersecting);
      },
      { rootMargin },
    );
    observer.observe(element);
    return () => observer.disconnect();
  }, [ref, rootMargin]);
  return inView;
}

/**
 * Whether the browser is skipping the section `element` is in (`.first-view` and `.skip-offscreen` in styles/site.tailwind).
 * Measuring in there forces the very layout that was skipped; a ResizeObserver reports when the section is laid out.
 */
export function isSkipped(element: Element): boolean {
  return typeof element.checkVisibility === "function" && !element.checkVisibility({ contentVisibilityAuto: true });
}

/**
 * A figure drawn one viewBox unit to the CSS pixel. `ref` goes on the box the
 * figure fills; `width` is that box's inner width (`fallback` until measured, as
 * in the server's HTML). A scaled viewBox shrinks its text to a few pixels on a
 * phone and swells it past the page's headings on a wide screen; laid out across
 * the measured width instead, text keeps the size it is set at. Measured before
 * the first paint (or, in a skipped section, as soon as it is laid out), and
 * again whenever the box resizes.
 */
export function useFigureWidth(fallback: number): { ref: RefObject<HTMLDivElement | null>; width: number } {
  const ref = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(fallback);
  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return;
    const measure = () => {
      const next = element.clientWidth;
      if (next > 0) setWidth(next);
    };
    const observes = typeof ResizeObserver !== "undefined";
    if (!observes || !isSkipped(element)) measure();
    if (!observes) return;
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  return { ref, width };
}
