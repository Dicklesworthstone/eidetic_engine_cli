import { ArrowRight, ExternalLink } from "lucide-react";

import { RecallTitle } from "@/components/instrument";
import { COUNTED, siteConfig } from "@/lib/content";

/** The page's foot: the closing line, the way to install, and the project's links. */
export function SiteFooter({ installHref = "#install" }: { installHref?: string }) {
  return (
    <footer data-outside className="dock-clearance relative isolate overflow-hidden border-t border-white/[0.06]">
      <div aria-hidden="true" className="field-light absolute inset-0 -z-10" />
      <div className="mx-auto max-w-7xl px-4 pt-24 pb-14 sm:px-6 md:pt-32">
        <div className="grid grid-cols-1 gap-12 lg:grid-cols-12">
          <div className="lg:col-span-7">
            <p className="display-2 font-extrabold text-white">
              <RecallTitle text="Give your agents a long-term [memory]." />
            </p>
            <p className="mt-6 max-w-2xl text-[15px] leading-relaxed text-slate-400">
              ee is released under the MIT license with an OpenAI/Anthropic rider. Its decision records, JSON schemas and
              failure-mode fixtures are all in the repository.
            </p>
            <div className="mt-8 flex flex-wrap gap-3">
              <a
                href={installHref}
                className="inline-flex min-h-12 items-center gap-2 rounded-2xl bg-violet-400 px-5 text-[15px] font-extrabold text-black transition-colors hover:bg-violet-300"
              >
                Install ee <ArrowRight className="h-4 w-4" aria-hidden="true" />
              </a>
              <a
                href={siteConfig.github}
                className="inline-flex min-h-12 items-center gap-2 rounded-2xl border border-white/15 bg-white/[0.02] px-5 text-[15px] font-bold text-white transition-colors hover:border-violet-400/50 hover:text-violet-200"
              >
                Open the repository <ExternalLink className="h-4 w-4" aria-hidden="true" />
              </a>
            </div>
          </div>
          <nav aria-label="Project" className="lg:col-span-5">
            <ul className="grid grid-cols-1 gap-2.5 text-sm min-[420px]:grid-cols-2">
              <FooterLink href={siteConfig.github} label="Repository" />
              <FooterLink href={siteConfig.readme} label="README" />
              <FooterLink href={siteConfig.changelog} label="Changelog" />
              <FooterLink href={siteConfig.adrs} label="Decision records" />
              <FooterLink href={siteConfig.schemas} label="JSON schemas" />
              <FooterLink href={siteConfig.trustModel} label="Trust model" />
              <FooterLink href={siteConfig.crate} label="crates.io" />
              <FooterLink href={siteConfig.license} label="License (MIT + rider)" />
            </ul>
          </nav>
        </div>
        <p className="mt-16 flex flex-wrap items-center gap-x-6 gap-y-2 border-t border-white/[0.06] pt-6 font-mono text-[11px] text-slate-500">
          <span>{siteConfig.name} · ee</span>
          <span>Counts taken from the repository</span>
          <span>
            Counted at {COUNTED.commit}, {COUNTED.date}
          </span>
        </p>
      </div>
    </footer>
  );
}

function FooterLink({ href, label }: { href: string; label: string }) {
  return (
    <li>
      <a
        href={href}
        className="flex min-h-12 items-center justify-between gap-2 rounded-xl border border-white/[0.08] bg-white/[0.015] px-4 text-slate-300 transition-colors hover:border-violet-400/40 hover:text-violet-200"
      >
        {label} <ExternalLink className="h-3.5 w-3.5" aria-hidden="true" />
      </a>
    </li>
  );
}
