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
        el.style.setProperty("--delay", `${Math.min(i, 6) * 0.08}s`);
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

  // The figures count up once, when they come into view.
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

    function place() {
      queued = false;
      const middle = window.innerHeight * 0.5;
      for (let i = 0; i < sections.length; i += 1) {
        const box = sections[i].getBoundingClientRect();
        if (box.top <= middle && box.bottom > middle) {
          const p = (middle - box.top) / box.height;
          const next = sections[i + 1] || sections[i];
          root.classList.toggle("veil-right", sections[i].classList.contains("right"));
          if (scene) scene.setScroll(sections[i].dataset.scene, p, next.dataset.scene);
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
    window.addEventListener("resize", onScroll, { passive: true });
    place();

    if (reduced || !webgl2()) {
      root.classList.add("no-scene");
      return;
    }
    import("./scene.js")
      .then(({ startScene }) => {
        scene = startScene(canvas, {
          tags: document.querySelector(".stage .tags"),
          onReady: () => requestAnimationFrame(() => root.classList.add("scene-on")),
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
