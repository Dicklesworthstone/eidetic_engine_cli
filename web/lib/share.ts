// The share images (app/opengraph-image.tsx, app/twitter-image.tsx). The static
// export writes them without an extension, so worker.ts gives them their type.

export const SHARE_IMAGE_TYPE = "image/png";
export const SHARE_IMAGE_PATHS: readonly string[] = ["/opengraph-image", "/twitter-image"];
