import type { NextConfig } from "next";

// A static export: `next build` writes `out/`, which Cloudflare serves as a
// Worker's static assets (wrangler.jsonc). Nothing runs on a server.
const nextConfig: NextConfig = {
  output: "export",
  images: { unoptimized: true },
  poweredByHeader: false,
  reactStrictMode: true,
  // `next dev` otherwise writes web/AGENTS.md when it sees a coding agent;
  // the repository's AGENTS.md is the agents' contract.
  agentRules: false,
};

export default nextConfig;
