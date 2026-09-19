import { plugin } from "bun";
import { compile } from "svelte/compiler";

/**
 * Compiles `.svelte` files (runes mode) inside Bun.build — the same approach
 * as moonshine's svelte-adopt example: markup is compiled by the real Svelte
 * compiler, moonshine hosts the pieces around it.
 */
export const sveltePlugin = (generate: "server" | "client") => ({
  name: "darash-svelte",
  setup(build: any) {
    build.onLoad({ filter: /\.svelte$/ }, async (args: any) => {
      const source = await Bun.file(args.path).text();
      const { js } = compile(source, {
        filename: args.path,
        generate,
        runes: true,
      });
      return { contents: js.code, loader: "js" };
    });
  },
});

plugin(sveltePlugin("server"));
