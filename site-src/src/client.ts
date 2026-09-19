/**
 * Client entry: mounts the Svelte islands onto the `.crepus`-rendered shell
 * and drives the header logo reveal from a moonshine signal.
 */
import { mount } from "svelte";
import { state, effect } from "@tschk/moonshine/runes";
import Console from "../islands/Console.svelte";
import LivePanel from "../islands/LivePanel.svelte";
import { startLiveFeed } from "./live";

const mounts: Record<string, typeof Console | typeof LivePanel> = {
  console: Console,
  live: LivePanel,
};

document.querySelectorAll<HTMLElement>("[data-island]").forEach((el) => {
  const name = el.dataset.island ?? "";
  const component = mounts[name];
  if (!component) return;
  el.innerHTML = "";
  mount(component as never, { target: el });
});

// Header logo appears only after scrolling past the hero; nav links stay put.
const hero = document.getElementById("hero");
const logoVisible = state(false);

effect(() => {
  const header = document.querySelector<HTMLElement>("header");
  header?.classList.toggle("logo-on", logoVisible());
});

if (hero && typeof IntersectionObserver !== "undefined") {
  const observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) logoVisible.set(!entry.isIntersecting);
    },
    { rootMargin: "-72px 0px 0px 0px", threshold: 0 },
  );
  observer.observe(hero);
} else {
  // No hero (or no IO): never hide the mark.
  logoVisible.set(true);
}

startLiveFeed();
