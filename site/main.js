// Zecured's landing page: the reveals, the counters, and the scene (scene.js, loaded once the page is up and only
// where WebGL 2 is available and motion is welcome). Without this script every word of the page still shows.
"use strict";

document.documentElement.classList.add("js");

(function () {
  const root = document.documentElement;
  const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  function webgl2() {
    try {
      return !!document.createElement("canvas").getContext("webgl2");
    } catch {
      return false;
    }
  }

  // A custom property on the page's root, written only when it changes (each write restyles the whole page).
  const written = new Map();
  function setVar(name, value) {
    if (written.get(name) === value) return;
    written.set(name, value);
    root.style.setProperty(name, value);
  }

  // Each element with .reveal (and the headline) comes in once it enters the view.
  function setUpReveals() {
    const items = document.querySelectorAll(".reveal, .display");
    if (reduced || !("IntersectionObserver" in window)) {
      items.forEach((el) => el.classList.add("in"));
      return;
    }
    // Siblings that arrive together come in one after another.
    document.querySelectorAll(".panel, .hero-in, .stats, .doors, .versus").forEach((group) => {
      group.querySelectorAll(":scope > .reveal, :scope > li.reveal, :scope > div.reveal").forEach((el, i) => {
        el.style.setProperty("--delay", `${Math.min(i, 6) * 0.07}s`);
      });
    });
    const seen = new IntersectionObserver(
      (entries) => {
        entries.forEach((entry) => {
          if (!entry.isIntersecting) return;
          entry.target.classList.add("in");
          seen.unobserve(entry.target);
        });
      },
      { rootMargin: "0px 0px -8% 0px", threshold: 0.12 },
    );
    items.forEach((el) => seen.observe(el));
  }

  // The figures count up once, when they come into view. Each counting figure is hidden from screen readers, which
  // read its final value from the label beside it.
  function setUpCounters() {
    const figures = Array.from(document.querySelectorAll(".stat-n[data-count]"));
    if (reduced || !("IntersectionObserver" in window)) return;
    const run = (el) => {
      const end = Number(el.dataset.count);
      const decimals = Number(el.dataset.decimals || 0);
      const begin = performance.now();
      const step = (now) => {
        const t = Math.min(1, (now - begin) / 1600);
        const eased = 1 - Math.pow(2, -10 * t);
        el.textContent = (end * (t === 1 ? 1 : eased)).toLocaleString("en-US", {
          minimumFractionDigits: decimals,
          maximumFractionDigits: decimals,
        });
        if (t < 1) requestAnimationFrame(step);
      };
      requestAnimationFrame(step);
    };
    const seen = new IntersectionObserver(
      (entries) => {
        entries.forEach((entry) => {
          if (!entry.isIntersecting) return;
          run(entry.target);
          seen.unobserve(entry.target);
        });
      },
      { threshold: 0.6 },
    );
    figures.forEach((el) => seen.observe(el));
  }

  // The header gains a ground once the page moves.
  function setUpHeader() {
    const top = document.querySelector(".top");
    if (!top) return;
    const update = () => top.classList.toggle("scrolled", window.scrollY > 24);
    update();
    window.addEventListener("scroll", update, { passive: true });
  }

  // The scene follows the chapter in the middle of the screen.
  function setUpScene() {
    const canvas = document.getElementById("scene");
    const sections = Array.from(document.querySelectorAll("[data-scene]"));
    if (!canvas || sections.length === 0) return;
    let scene = null;
    let queued = false;

    // Where a section's text ends, as a share of the screen: its right edge on a wide screen, its top on a tall one
    // (from the top of its own screen, so neither the scroll nor a reveal's transform moves it). The scene stands its
    // story in the space left over; 0 when the text runs across the screen.
    const TEXT = ".panel > *, .hero-in > .eyebrow, .hero-in > .lead, .hero-in > .proofline, .display .line > span, .ctas > *";
    const edges = new Map();
    function edgeOf(section) {
      if (!edges.has(section)) {
        const tall = window.innerWidth / window.innerHeight < 0.9;
        let edge = tall ? 1 : 0;
        section.querySelectorAll(TEXT).forEach((el) => {
          // A picture only some readers get takes no room when it is not shown.
          if (el.offsetParent === null) return;
          if (tall) edge = Math.min(edge, el.offsetTop / window.innerHeight);
          else edge = Math.max(edge, el.getBoundingClientRect().right / window.innerWidth);
        });
        edges.set(section, edge === 1 ? 0 : edge);
      }
      return edges.get(section);
    }

    function place() {
      queued = false;
      const middle = window.innerHeight * 0.5;
      for (let i = 0; i < sections.length; i += 1) {
        const box = sections[i].getBoundingClientRect();
        if (box.top <= middle && box.bottom > middle) {
          const p = (middle - box.top) / box.height;
          root.classList.toggle("veil-wide", sections[i].dataset.veil === "wide");
          if (scene) {
            const edge = edgeOf(sections[i]);
            scene.setScroll(sections[i].dataset.scene, p, edge);
            // On a tall screen the veil darkens from where the text begins, on a wide one up to where its lines end
            // (landing.css).
            const tall = window.innerWidth / window.innerHeight < 0.9;
            root.classList.toggle("text-top", tall && edge > 0);
            if (tall && edge > 0) setVar("--text-top", edge.toFixed(3));
            if (!tall) setVar("--text-edge", edge.toFixed(3));
          }
          return;
        }
      }
    }

    const onScroll = () => {
      if (!queued) {
        queued = true;
        requestAnimationFrame(place);
      }
    };
    window.addEventListener("scroll", onScroll, { passive: true });
    window.addEventListener("resize", () => {
      edges.clear();
      onScroll();
    }, { passive: true });
    place();

    if (reduced || !webgl2()) {
      root.classList.add("no-scene");
      return;
    }
    // The scene writes on its things with the page's fonts: it waits for them.
    const fonts = document.fonts ? document.fonts.ready : Promise.resolve();
    fonts
      .then(() => import("./scene.js"))
      .then(({ startScene }) => {
        scene = startScene(canvas, {
          tags: document.querySelector(".stage .tags"),
          onReady: () => requestAnimationFrame(() => root.classList.add("scene-on")),
          // If the GPU drops the scene, the still image comes back in its place.
          onLost: () => root.classList.remove("scene-on"),
        });
        if (!scene) root.classList.add("no-scene");
        else place();
      })
      .catch(() => root.classList.add("no-scene"));
  }

  document.addEventListener("DOMContentLoaded", () => {
    setUpReveals();
    setUpCounters();
    setUpHeader();
    setUpScene();
  });
})();
