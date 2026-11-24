"use client";

import { useEffect } from "react";

import { isSkipped } from "@/lib/hooks";

/**
 * Arms every <Reveal> on the page once: blocks already on screen stay as
 * they are, and each one below waits, hidden, until it scrolls into view.
 * Nothing is armed for a reader who asks for less motion, or without
 * IntersectionObserver.
 */
export function RevealController() {
  useEffect(() => {
    if (typeof IntersectionObserver === "undefined") return;
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    const blocks = Array.from(document.querySelectorAll<HTMLElement>(".reveal"));
    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (!entry.isIntersecting) continue;
          entry.target.classList.add("is-shown");
          observer.unobserve(entry.target);
        }
      },
      { rootMargin: "0px 0px -10% 0px", threshold: 0.05 },
    );
    for (const block of blocks) {
      // Already visible (the reader is looking at it): leave it, with no entrance. A skipped one is far below.
      if (!isSkipped(block) && block.getBoundingClientRect().top < window.innerHeight) {
        block.classList.add("is-shown");
      } else {
        observer.observe(block);
      }
    }
    document.documentElement.classList.add("reveal-armed");
    return () => {
      observer.disconnect();
      document.documentElement.classList.remove("reveal-armed");
    };
  }, []);
  return null;
}
