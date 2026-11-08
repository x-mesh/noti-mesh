"use strict";
(() => {
  const bound = new WeakSet();
  const once = (element) => (bound.has(element) ? false : (bound.add(element), true));
  const emit = (element, detail) => element.dispatchEvent(new CustomEvent("jk:change", { bubbles: true, detail }));
  const announce = (container, message) => {
    const target = container.dataset.jkStatus && document.querySelector(container.dataset.jkStatus);
    if (target) target.textContent = message;
  };
  const moveKeys = (event, index, length, vertical) => ({
    [vertical ? "ArrowDown" : "ArrowRight"]: (index + 1) % length,
    [vertical ? "ArrowUp" : "ArrowLeft"]: (index + length - 1) % length,
    Home: 0,
    End: length - 1,
  })[event.key];

  function tabs(list) {
    const items = [...list.querySelectorAll("[role=tab]")];
    const select = (tab) => {
      items.forEach((item) => {
        const active = item === tab;
        item.setAttribute("aria-selected", String(active));
        item.tabIndex = active ? 0 : -1;
        const panel = document.getElementById(item.getAttribute("aria-controls"));
        if (panel) panel.hidden = !active;
      });
      emit(list, { value: tab.id });
    };
    items.forEach((tab, index) => {
      tab.addEventListener("click", () => select(tab));
      tab.addEventListener("keydown", (event) => {
        const target = moveKeys(event, index, items.length, false);
        if (target === undefined) return;
        event.preventDefault();
        select(items[target]);
        items[target].focus();
      });
    });
  }

  // data-jk-dismiss: Escape or a click outside closes it and focus returns to the button.
  // A panel with data-jk-animate stays in the DOM, gets data-open and becomes inert while closed.
  function disclosure(button) {
    const panels = () => button.getAttribute("aria-controls").split(/\s+/).map((id) => document.getElementById(id)).filter(Boolean);
    const set = (open) => {
      button.setAttribute("aria-expanded", String(open));
      // aria-controls may list several ids, e.g. the child rows of a tree table.
      panels().forEach((panel) => {
        if (panel.hasAttribute("data-jk-animate")) { panel.toggleAttribute("data-open", open); panel.inert = !open; }
        else panel.hidden = !open;
      });
      emit(button, { open });
    };
    panels().forEach((panel) => { if (panel.hasAttribute("data-jk-animate")) panel.inert = button.getAttribute("aria-expanded") !== "true"; });
    button.addEventListener("click", () => set(button.getAttribute("aria-expanded") !== "true"));
    if (!button.hasAttribute("data-jk-dismiss")) return;
    document.addEventListener("keydown", (event) => {
      if (event.key !== "Escape" || button.getAttribute("aria-expanded") !== "true") return;
      set(false);
      button.focus();
    });
    document.addEventListener("click", (event) => {
      if (button.getAttribute("aria-expanded") !== "true" || button.contains(event.target) || panels().some((panel) => panel.contains(event.target))) return;
      set(false);
    });
  }

  // Buttons with data-jk-filter-value narrow the items of aria-controls whose data-jk-filter lists that value.
  // "all" shows everything. Works with role="tab" (aria-selected, arrow keys) or plain aria-pressed buttons.
  function filter(group) {
    const buttons = [...group.querySelectorAll("[data-jk-filter-value]")];
    const target = document.getElementById(group.getAttribute("aria-controls"));
    if (!target) return;
    const asTabs = buttons[0]?.getAttribute("role") === "tab";
    const select = (button) => {
      buttons.forEach((item) => {
        const active = item === button;
        item.setAttribute(asTabs ? "aria-selected" : "aria-pressed", String(active));
        if (asTabs) item.tabIndex = active ? 0 : -1;
      });
      const value = button.dataset.jkFilterValue;
      const items = [...target.querySelectorAll("[data-jk-filter]")];
      items.forEach((item) => { item.hidden = value !== "all" && !item.dataset.jkFilter.split(/\s+/).includes(value); });
      const shown = items.filter((item) => !item.hidden).length;
      announce(group, (group.dataset.jkCountFormat || "{n}개").replace("{n}", shown));
      emit(group, { value, shown });
    };
    buttons.forEach((button, index) => {
      button.addEventListener("click", () => select(button));
      if (!asTabs) return;
      button.addEventListener("keydown", (event) => {
        const next = moveKeys(event, index, buttons.length, false);
        if (next === undefined) return;
        event.preventDefault();
        select(buttons[next]);
        buttons[next].focus();
      });
    });
  }

  // Lines inside data-jk="reveal" get data-lit while they cross the middle band of the viewport.
  function reveal(box) {
    const lines = [...box.querySelectorAll("[data-jk-reveal-line]")];
    if (!("IntersectionObserver" in window)) { lines.forEach((line) => line.setAttribute("data-lit", "")); return; }
    const observer = new IntersectionObserver((entries) => entries.forEach((entry) => entry.target.toggleAttribute("data-lit", entry.isIntersecting)), { rootMargin: "-38% 0px -38% 0px" });
    lines.forEach((line) => observer.observe(line));
  }

  // data-jk-mode="single" keeps exactly one button pressed, like a segmented choice.
  function pressed(group) {
    const single = group.dataset.jkMode === "single";
    group.addEventListener("click", (event) => {
      const button = event.target.closest("button[aria-pressed]");
      if (!button || !group.contains(button) || button.disabled) return;
      const buttons = [...group.querySelectorAll("button[aria-pressed]")];
      if (single) buttons.forEach((item) => item.setAttribute("aria-pressed", String(item === button)));
      else button.setAttribute("aria-pressed", String(button.getAttribute("aria-pressed") !== "true"));
      const values = buttons.filter((item) => item.getAttribute("aria-pressed") === "true").map((item) => item.value || item.textContent.trim());
      emit(group, { value: button.value || button.textContent.trim(), pressed: button.getAttribute("aria-pressed") === "true", values });
    });
  }

  // Removing a token moves focus to its neighbor so keyboard users are not dropped to <body>.
  function tokens(group) {
    group.addEventListener("click", (event) => {
      const button = event.target.closest(".jk-token button");
      if (!button || !group.contains(button)) return;
      const token = button.closest(".jk-token");
      const removeButtons = [...group.querySelectorAll(".jk-token button")];
      const index = removeButtons.indexOf(button);
      const label = token.textContent.trim().replace(/\s+/g, " ");
      token.remove();
      const rest = [...group.querySelectorAll(".jk-token button")];
      const next = rest[index] || rest[index - 1] || group.querySelector("[data-jk-fallback]");
      if (next) next.focus();
      announce(group, `${label} 조건을 지웠습니다. 남은 조건 ${rest.length}개`);
      emit(group, { removed: label, count: rest.length });
    });
  }

  function listbox(box) {
    const options = [...box.querySelectorAll("[role=option]")];
    const select = (option) => {
      options.forEach((item) => {
        const active = item === option;
        item.setAttribute("aria-selected", String(active));
        item.tabIndex = active ? 0 : -1;
      });
      emit(box, { value: option.dataset.value ?? option.textContent.trim(), option });
    };
    options.forEach((option, index) => {
      option.addEventListener("click", () => select(option));
      option.addEventListener("keydown", (event) => {
        if (event.key === "Enter" || event.key === " ") { event.preventDefault(); select(option); return; }
        const target = moveKeys(event, index, options.length, true);
        if (target === undefined) return;
        event.preventDefault();
        select(options[target]);
        options[target].focus();
      });
    });
  }

  // <input data-jk-checkall="docs"> drives every checkbox named "docs" and shows a mixed state.
  function checkall(master) {
    const members = () => [...document.querySelectorAll(`input[type=checkbox][name="${CSS.escape(master.dataset.jkCheckall)}"]`)].filter((input) => !input.disabled);
    const sync = () => {
      const list = members();
      const checked = list.filter((input) => input.checked).length;
      master.checked = checked > 0 && checked === list.length;
      master.indeterminate = checked > 0 && checked < list.length;
      return checked;
    };
    master.addEventListener("change", () => {
      members().forEach((input) => { input.checked = master.checked; });
      emit(master, { checked: sync() });
    });
    document.addEventListener("change", (event) => {
      if (event.target !== master && members().includes(event.target)) emit(master, { checked: sync() });
    });
    sync();
  }

  function count(field) {
    const output = document.querySelector(field.dataset.jkTarget);
    const update = () => { if (output) output.textContent = `${field.value.length} / ${field.maxLength}`; };
    field.addEventListener("input", update);
    update();
  }

  // Opener: <button data-jk="dialog" aria-controls="dialog-id">. Inside the dialog, [data-jk-close] closes it
  // with its value; focus returns to the opener because showModal() restores it on close.
  function dialog(opener) {
    const target = document.getElementById(opener.getAttribute("aria-controls"));
    if (!target) return;
    opener.addEventListener("click", () => target.showModal());
    if (!once(target)) return;
    target.addEventListener("click", (event) => {
      const close = event.target.closest("[data-jk-close]");
      if (close) target.close(close.value || "");
      else if (event.target === target) target.close("");
    });
    target.addEventListener("close", () => emit(target, { value: target.returnValue }));
  }

  // <div data-jk-bulk="rows" hidden> appears while any checkbox named "rows" is checked;
  // [data-jk-bulk-count] inside it shows how many.
  function bulk(bar) {
    const name = bar.dataset.jkBulk;
    const update = () => {
      const checked = document.querySelectorAll(`input[type=checkbox][name="${CSS.escape(name)}"]:checked`).length;
      bar.hidden = checked === 0;
      bar.querySelectorAll("[data-jk-bulk-count]").forEach((output) => { output.textContent = String(checked); });
    };
    document.addEventListener("change", (event) => { if (event.target.name === name || event.target.dataset?.jkCheckall === name) update(); });
    bar.addEventListener("click", (event) => {
      if (!event.target.closest("[data-jk-bulk-clear]")) return;
      document.querySelectorAll(`input[type=checkbox][name="${CSS.escape(name)}"], input[data-jk-checkall="${CSS.escape(name)}"]`).forEach((input) => { input.checked = false; input.indeterminate = false; });
      update();
    });
    update();
  }

  // <button class="jk-status" data-jk="cycle" data-jk-values="todo:시작 전,doing:진행 중,done:완료">
  function cycle(button) {
    const steps = button.dataset.jkValues.split(",").map((pair) => pair.split(":"));
    button.addEventListener("click", () => {
      const index = steps.findIndex(([value]) => value === button.dataset.status);
      const [value, label] = steps[(index + 1) % steps.length];
      button.dataset.status = value;
      button.textContent = label;
      emit(button, { value, label });
    });
  }

  // A form with one text input, or a plain button, appends a copy of <template data-jk-template>
  // to data-jk-target. In the copy, [data-jk-slot] receives the typed text.
  function append(source) {
    const list = document.querySelector(source.dataset.jkTarget);
    const template = document.querySelector(source.dataset.jkTemplate);
    if (!list || !template) return;
    const add = (event) => {
      event.preventDefault();
      const input = source.matches("form") ? source.querySelector("input") : null;
      const text = input ? input.value.trim() : "";
      if (input && !text) { input.focus(); return; }
      const item = template.content.firstElementChild.cloneNode(true);
      item.querySelectorAll("[data-jk-slot]").forEach((slot) => { slot.textContent = text; });
      list.append(item);
      init(item);
      if (input) { input.value = ""; input.focus(); }
      else item.querySelector("button, input, select, textarea, [tabindex]")?.focus();
      announce(source, text ? `${text}을(를) 추가했습니다. 모두 ${list.children.length}개` : `항목을 추가했습니다. 모두 ${list.children.length}개`);
      emit(source, { added: text, count: list.children.length });
    };
    source.addEventListener(source.matches("form") ? "submit" : "click", add);
  }

  // Marks empty (or badly formatted email) required fields with aria-invalid, reveals their error text and focuses the first one.
  function validate(form) {
    form.noValidate = true;
    const errorFor = (field) => document.getElementById((field.getAttribute("aria-describedby") || "").split(/\s+/).find((id) => document.getElementById(id)?.matches("[data-jk-error]")) || "");
    const isEmpty = (field) => (field.type === "radio" ? !form.querySelector(`input[name="${CSS.escape(field.name)}"]:checked`) : field.type === "checkbox" ? !field.checked : !field.value.trim() || (field.type === "email" && !field.checkValidity()));
    const setInvalid = (field, invalid) => {
      field.setAttribute("aria-invalid", String(invalid));
      const error = errorFor(field);
      if (error) error.hidden = !invalid;
    };
    form.addEventListener("submit", (event) => {
      event.preventDefault();
      const fields = [...form.querySelectorAll("[required]")];
      const invalid = fields.filter(isEmpty);
      fields.forEach((field) => setInvalid(field, invalid.includes(field)));
      if (invalid.length) {
        invalid[0].focus();
        emit(form, { valid: false, invalid: invalid.length });
        return;
      }
      if (form.dataset.jkSuccess) announce(form, form.dataset.jkSuccess);
      emit(form, { valid: true, data: Object.fromEntries(new FormData(form)) });
    });
    form.addEventListener("input", (event) => {
      if (event.target.getAttribute("aria-invalid") === "true" && !isEmpty(event.target)) setInvalid(event.target, false);
    });
  }

  const reducedMotion = matchMedia("(prefers-reduced-motion: reduce)");

  // Effects never autoplay: a button with data-jk="fx" and aria-controls starts and stops them, and they
  // pause while scrolled out of view. Stages with data-jk-entrance play once and stop at their final frame.
  function fx(button) {
    const stage = document.getElementById(button.getAttribute("aria-controls"));
    if (!stage) return;
    const entrance = stage.hasAttribute("data-jk-entrance");
    const [playLabel, stopLabel] = (button.dataset.jkLabels || "재생|정지").split("|");
    const label = button.querySelector("[data-jk-fx-label]") || button;
    const render = () => {
      const playing = stage.dataset.playing === "true";
      button.setAttribute("aria-pressed", String(playing));
      label.textContent = entrance ? playLabel : playing ? stopLabel : playLabel;
      const icon = button.querySelector("[data-jk-fx-icon]");
      if (icon) icon.textContent = playing && !entrance ? "Ⅱ" : "▷";
    };
    if (once(stage) && "IntersectionObserver" in window) {
      new IntersectionObserver(([entry]) => { stage.toggleAttribute("data-offscreen", !entry.isIntersecting); }).observe(stage);
    }
    if (entrance) {
      stage.addEventListener("animationend", (event) => {
        if (!event.target.classList.contains("jk-fx-entry-complete")) return;
        stage.dataset.playing = "false";
        render();
        emit(stage, { playing: false, finished: true });
      });
    }
    button.addEventListener("click", () => {
      if (entrance) {
        stage.removeAttribute("data-started");
        void stage.offsetWidth;
        stage.dataset.started = "true";
        stage.dataset.playing = String(!reducedMotion.matches);
      } else {
        stage.dataset.playing = String(stage.dataset.playing !== "true");
      }
      render();
      emit(stage, { playing: stage.dataset.playing === "true", reducedMotion: reducedMotion.matches });
    });
    render();
  }

  // Types phrases from data-jk-phrases ("a|b|c") into [data-jk-typing-text], deletes them, and moves on.
  // A button with data-jk="typing-toggle" and aria-controls pauses it; reduced motion shows the first phrase.
  const typingState = new WeakMap();
  function typing(box) {
    const output = box.querySelector("[data-jk-typing-text]");
    const phrases = box.dataset.jkPhrases.split("|");
    const state = { index: 0, length: 0, deleting: false, timer: 0, paused: false };
    typingState.set(box, state);
    const tick = () => {
      const full = phrases[state.index];
      if (reducedMotion.matches || state.paused) { output.textContent = full; return; }
      state.length += state.deleting ? -1 : 1;
      output.textContent = full.slice(0, state.length);
      let delay = state.deleting ? 28 : 70;
      if (!state.deleting && state.length === full.length) { state.deleting = true; delay = 1800; }
      else if (state.deleting && state.length === 0) { state.deleting = false; state.index = (state.index + 1) % phrases.length; delay = 400; }
      state.timer = setTimeout(tick, delay);
    };
    box.jkTypingRestart = () => { clearTimeout(state.timer); state.length = 0; state.deleting = false; tick(); };
    reducedMotion.addEventListener("change", box.jkTypingRestart);
    tick();
  }
  function typingToggle(button) {
    const box = document.getElementById(button.getAttribute("aria-controls"));
    const [pauseLabel, resumeLabel] = (button.dataset.jkLabels || "입력 멈추기|입력 다시 보기").split("|");
    button.addEventListener("click", () => {
      const state = typingState.get(box);
      if (!state) return;
      state.paused = !state.paused;
      button.setAttribute("aria-pressed", String(state.paused));
      button.textContent = state.paused ? resumeLabel : pauseLabel;
      box.jkTypingRestart();
      emit(button, { paused: state.paused });
    });
  }

  const behaviors = { tabs, disclosure, pressed, tokens, listbox, count, dialog, cycle, append, validate, fx, typing, "typing-toggle": typingToggle, filter, reveal };
  function init(root = document) {
    root.querySelectorAll("[data-jk]").forEach((element) => {
      const behavior = behaviors[element.dataset.jk];
      if (behavior && once(element)) behavior(element);
    });
    root.querySelectorAll("input[data-jk-checkall]").forEach((input) => { if (once(input)) checkall(input); });
    root.querySelectorAll("[data-jk-bulk]").forEach((bar) => { if (once(bar)) bulk(bar); });
  }

  window.jk = { init };
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", () => init());
  else init();
})();
