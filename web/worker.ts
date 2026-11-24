// The Worker in front of the site (wrangler.jsonc's `run_worker_first`). It
// sends www.eidetic-engine.org to eidetic-engine.org, gives the share images
// their type (the static export writes them without an extension), and hands
// everything else to the static assets unchanged.

import { SHARE_IMAGE_PATHS, SHARE_IMAGE_TYPE } from "./lib/share";

type Env = { ASSETS: { fetch(request: Request): Promise<Response> } };

const APEX = "eidetic-engine.org";

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    if (url.hostname === `www.${APEX}`) {
      url.hostname = APEX;
      return Response.redirect(url.toString(), 301);
    }
    const response = await env.ASSETS.fetch(request);
    if (SHARE_IMAGE_PATHS.includes(url.pathname) && response.ok) {
      const headers = new Headers(response.headers);
      headers.set("Content-Type", SHARE_IMAGE_TYPE);
      headers.set("Cache-Control", "public, max-age=86400");
      return new Response(response.body, { status: response.status, headers });
    }
    return response;
  },
};
