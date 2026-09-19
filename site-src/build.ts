/**
 * darash site build.
 *
 * 1. `crepus render index.crepus` renders the static page shell (crepuscularity).
 * 2. Svelte islands (Console, LivePanel) are server-rendered with svelte/server
 *    and injected into their [data-island] mounts, so the page paints fully
 *    formed without JS.
 * 3. Bun bundles the client entry (Svelte runes + @tschk/moonshine signals)
 *    which mounts/hydrates the islands and starts the live feed.
 * 4. Output goes to ../site — served by the Rust worker's assets pipeline.
 *
 * Literal braces cannot appear inside .crepus text nodes (the `{` starts an
 * interpolation), so the template authors @LB@ / @RB@ and we expand them here.
 */
import { $ } from "bun";
import { sveltePlugin } from "./svelte-plugin";

const root = import.meta.dir;
const outDir = `${root}/../site`;
const assetsDir = `${outDir}/assets`;

await $`mkdir -p ${assetsDir}`;

// 1 — static shell from crepuscularity.
const shell = await $`crepus render ${root}/index.crepus`.quiet().text();
const html = shell
  .replaceAll("@LB@", "{")
  .replaceAll("@RB@", "}")
  .trim();

// 2 — server-render the islands for first paint (no-JS still sees a page).
const serverEntry = `${root}/.cache/server-entry.ts`;
await Bun.write(
  serverEntry,
  `
import { render } from "svelte/server";
import Console from "../islands/Console.svelte";
import LivePanel from "../islands/LivePanel.svelte";
export const islands = {
  console: render(Console as never, { props: {} }),
  live: render(LivePanel as never, { props: {} }),
};
`,
);
const serverBuild = await Bun.build({
  entrypoints: [serverEntry],
  target: "bun",
  format: "esm",
  naming: "server.mjs",
  root: root,
  outdir: `${root}/.cache`,
  plugins: [sveltePlugin("server")],
});
if (!serverBuild.success) {
  console.error(serverBuild.logs);
  process.exit(1);
}
const { islands } = await import(`${root}/.cache/server.mjs`);

let document = html;
for (const [name, rendered] of Object.entries(islands as Record<string, { html: string; head?: string }>)) {
  const mount = `data-island="${name}"`;
  const index = document.indexOf(mount);
  if (index === -1) {
    console.error(`no mount for island "${name}"`);
    process.exit(1);
  }
  const openEnd = document.indexOf(">", index) + 1;
  const closeIndex = document.indexOf("</div>", openEnd);
  document =
    document.slice(0, openEnd) +
    rendered.html +
    (rendered.head ?? "") +
    document.slice(closeIndex);
}

// 3 — client bundle: Svelte islands on the moonshine signal kernel.
const clientBuild = await Bun.build({
  entrypoints: [`${root}/src/client.ts`],
  target: "browser",
  format: "esm",
  minify: true,
  naming: "islands.js",
  outdir: assetsDir,
  plugins: [sveltePlugin("client")],
  define: { "process.env.NODE_ENV": '"production"' },
});
if (!clientBuild.success) {
  console.error(clientBuild.logs);
  process.exit(1);
}

// 4 — assemble the final document.
const styles = await Bun.file(`${root}/styles.css`).text();
await Bun.write(
  `${root}/.cache/unocss-copy.js`,
  Bun.file(`${root}/vendor/unocss.js`),
);
await Bun.write(`${assetsDir}/unocss.js`, Bun.file(`${root}/vendor/unocss.js`));

const head = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>darash — all-in-one research crate</title>
<meta name="description" content="darash is the all-in-one research crate: async web search, page fetch, and HTML extraction in one Rust dependency. Key-free and deterministic — with a hosted search API at darash.tsc.hk.">
<link rel="canonical" href="https://darash.tsc.hk/">
<link rel="icon" href="/favicon.svg" type="image/svg+xml">
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link href="https://fonts.googleapis.com/css2?family=JetBrains+Mono:ital,wght@0,100..800;1,100..800&display=swap" rel="stylesheet">
<style>${styles}</style>
<script src="/assets/unocss.js"></script>
</head>
<body>
`;

const tail = `
<script type="module" src="/assets/islands.js"></script>
</body>
</html>
`;

await Bun.write(`${outDir}/index.html`, `${head}${document}${tail}`);
console.log(
  `site: index.html (${document.length}B shell) + assets/islands.js + assets/unocss.js`,
);
