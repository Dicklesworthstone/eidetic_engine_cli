"use client";

import { Check, Copy } from "lucide-react";
import { useState } from "react";

/** One shell command with a copy button. */
export function CopyCommand({ comment, command }: { comment: string; command: string }) {
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(command);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1600);
    } catch {
      // No clipboard (an insecure context, say): the command is still there to select.
    }
  };

  return (
    <div className="group panel rounded-2xl p-4 sm:p-5">
      <div className="mb-2 flex items-start justify-between gap-3">
        <p className="font-mono text-xs text-slate-500"># {comment}</p>
        <button
          type="button"
          onClick={copy}
          className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl border border-white/10 text-slate-400 transition hover:border-violet-400/50 hover:text-violet-200 focus-visible:ring-2 focus-visible:ring-violet-400 focus-visible:outline-none"
          aria-label={copied ? "Copied" : "Copy the command"}
        >
          {copied ? <Check className="h-4 w-4 text-violet-300" /> : <Copy className="h-4 w-4" />}
        </button>
        {/* A focused button's new name is often not read out; this is. */}
        <span className="sr-only" aria-live="polite">
          {copied ? "Copied to the clipboard" : ""}
        </span>
      </div>
      <pre className="overflow-x-auto text-[13px] leading-relaxed text-violet-100 [scrollbar-width:thin]">
        <code>{command}</code>
      </pre>
    </div>
  );
}
