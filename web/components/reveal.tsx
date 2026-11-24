import type { CSSProperties, ReactNode } from "react";

import { cn } from "@/lib/utils";

/**
 * A block that rises into place the first time it scrolls into view. The
 * HTML shows it as it is: only once <RevealController> has run does a block
 * below the fold wait to be seen, so a page whose JavaScript never runs is
 * still whole.
 */
export function Reveal({
  children,
  className,
  delay = 0,
}: {
  children: ReactNode;
  className?: string;
  delay?: number;
}) {
  const style = delay > 0 ? ({ "--reveal-delay": `${delay}s` } as CSSProperties) : undefined;
  return (
    <div className={cn("reveal", className)} style={style}>
      {children}
    </div>
  );
}
