// Zecured's showcase page: the loan's five steps, the viewing key and the bridge comparison. The page works
// without this script: every step and both comparisons then show, one after the other (styles.css).
"use strict";

document.documentElement.classList.add("js");

document.addEventListener("DOMContentLoaded", () => {
  setUpLoan();
  setUpSees();
});

// Arrow keys (and Home, End) move between the buttons of a group, as in a toolbar.
function arrowTarget(key, index, count) {
  const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[key];
  if (step !== undefined) return index + step >= 0 && index + step < count ? index + step : null;
  if (key === "Home") return 0;
  if (key === "End") return count - 1;
  return null;
}

function setUpLoan() {
  const section = document.getElementById("loan");
  const body = document.getElementById("loan-body");
  if (!section || !body) return;
  const buttons = Array.from(body.querySelectorAll(".stepbtn"));
  const steps = Array.from(body.querySelectorAll(".loan-step"));
  const live = document.getElementById("loan-live");
  let current = 0;

  function show(index, announce) {
    current = Math.max(0, Math.min(steps.length - 1, index));
    steps.forEach((step, i) => {
      step.hidden = i !== current;
    });
    buttons.forEach((button, i) => button.setAttribute("aria-pressed", String(i === current)));
    if (announce && live) {
      const label = steps[current].querySelector(".step-lab");
      live.textContent = label ? label.textContent : "";
    }
  }

  buttons.forEach((button, i) => {
    button.addEventListener("click", () => show(i, true));
    button.addEventListener("keydown", (event) => {
      const to = arrowTarget(event.key, i, buttons.length);
      if (to === null) return;
      event.preventDefault();
      buttons[to].focus();
      show(to, true);
    });
  });

  // Previous and next: the focus stays on the same button of the step now shown, or on the other one at an end.
  body.addEventListener("click", (event) => {
    const go = event.target.closest("[data-go]");
    if (!go || go.disabled) return;
    show(current + Number(go.dataset.go), true);
    const same = steps[current].querySelector(`[data-go="${go.dataset.go}"]`);
    const target = same && !same.disabled ? same : steps[current].querySelector("[data-go]:not([disabled])");
    if (target) target.focus();
  });

  // The viewing key: one switch, shown in every step. It clears what the borrower can read; the market
  // maker's output, sent with no outgoing viewing key, stays encrypted and says why.
  const keys = Array.from(body.querySelectorAll(".vk"));
  function setKey(on) {
    section.classList.toggle("vk-on", on);
    keys.forEach((key) => key.setAttribute("aria-pressed", String(on)));
    body.querySelectorAll(".enc.readable").forEach((value) => value.setAttribute("aria-hidden", String(!on)));
    body.querySelectorAll(".enc-sr").forEach((note) => {
      note.hidden = on;
    });
  }
  keys.forEach((key) => key.addEventListener("click", () => setKey(key.getAttribute("aria-pressed") !== "true")));

  // A link to one step (#loan-step-3) opens it. Followed from inside the page, the browser could not scroll to
  // a step that was still hidden, so the script does; on load the browser scrolls once the step shows.
  function fromHash(scroll) {
    const match = /^#loan-step-([1-5])$/.exec(window.location.hash);
    if (!match) return;
    show(Number(match[1]) - 1, false);
    if (scroll) steps[current].scrollIntoView();
  }
  window.addEventListener("hashchange", () => fromHash(true));
  show(0, false);
  fromHash(false);
}

function setUpSees() {
  const seg = document.querySelector("#sees .seg");
  if (!seg) return;
  const buttons = Array.from(seg.querySelectorAll("button"));
  const views = Array.from(document.querySelectorAll("#sees .sees-view"));

  function show(view) {
    buttons.forEach((button) => button.setAttribute("aria-pressed", String(button.dataset.view === view)));
    views.forEach((el) => {
      el.hidden = el.id !== `sees-${view}`;
    });
  }

  buttons.forEach((button, i) => {
    button.addEventListener("click", () => show(button.dataset.view));
    button.addEventListener("keydown", (event) => {
      const to = arrowTarget(event.key, i, buttons.length);
      if (to === null) return;
      event.preventDefault();
      buttons[to].focus();
      show(buttons[to].dataset.view);
    });
  });
  show("bridge");
}
