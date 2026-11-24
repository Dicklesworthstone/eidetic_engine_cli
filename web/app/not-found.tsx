import { BoltedPanel, MicroLabel, RecallTitle } from "@/components/instrument";
import { siteConfig } from "@/lib/content";

export const metadata = { title: `Not found · ${siteConfig.name}` };

export default function NotFound() {
  return (
    <main className="flex min-h-screen items-center justify-center px-4">
      <BoltedPanel className="max-w-lg p-10 text-center">
        <MicroLabel>404 · no such memory</MicroLabel>
        <h1 className="mt-5 text-4xl text-white">
          <RecallTitle text="Nothing was [remembered] here." />
        </h1>
        <p className="mt-4 text-sm leading-relaxed text-slate-400">
          This page was never written down. ee would have told you why it was not in the pack.
        </p>
        <a
          href="/"
          className="mt-8 inline-flex items-center rounded-2xl bg-violet-400 px-5 py-3 text-sm font-black text-black transition hover:bg-violet-300 focus-visible:ring-2 focus-visible:ring-violet-300 focus-visible:outline-none"
        >
          Back to the page
        </a>
      </BoltedPanel>
    </main>
  );
}
