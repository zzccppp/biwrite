/* BiWrite mainpage — language, lightbox, reveal, paragraph pairing. */
(function () {
  "use strict";

  var root = document.documentElement;
  var langButtons = Array.prototype.slice.call(document.querySelectorAll("[data-set-lang]"));
  var dialog = document.getElementById("lightbox");
  var status = document.getElementById("lang-status");
  var activeTrigger = null;
  var currentLang = root.getAttribute("data-lang") === "zh" ? "zh" : "en";

  var META = {
    en: {
      title: "BiWrite — One draft, two languages. Bilingual LaTeX & Markdown editor",
      desc: "A desktop editor for writing academic papers in English and Chinese at once. Paragraph-by-paragraph translation, LaTeX and PDF side by side, and a writing assistant that follows the rules of research-builder."
    },
    zh: {
      title: "BiWrite — 一篇稿子，两种语言｜中英双语论文编辑器",
      desc: "中英双语论文写作桌面编辑器。两侧逐段对照，只重译改动过的段落；LaTeX 与 PDF 并排编译；写作助手遵循 research-builder 的规则。"
    }
  };

  function safeStorageGet(key) {
    try {
      return localStorage.getItem(key);
    } catch (e) {
      return null;
    }
  }

  function safeStorageSet(key, value) {
    try {
      localStorage.setItem(key, value);
    } catch (e) {
      /* storage unavailable; the choice just won't persist */
    }
  }

  function setUrlLang(lang) {
    if (!/^https?:$/.test(location.protocol)) return;
    try {
      var url = new URL(location.href);
      if (url.searchParams.get("lang") === lang) return;
      url.searchParams.set("lang", lang);
      history.replaceState(null, "", url.toString());
    } catch (e) {
      /* ignore */
    }
  }

  function updateImage(img, lang) {
    var srcKey = lang === "zh" ? "srcZh" : "srcEn";
    var srcsetKey = lang === "zh" ? "srcsetZh" : "srcsetEn";
    var altKey = lang === "zh" ? "altZh" : "altEn";
    var src = img.dataset[srcKey];
    var srcset = img.dataset[srcsetKey];
    var alt = img.dataset[altKey];

    if (srcset) img.setAttribute("srcset", srcset);
    if (src) img.setAttribute("src", src);
    if (alt) img.setAttribute("alt", alt);
  }

  function fillLightbox(trigger, lang) {
    if (!dialog || !trigger) return;
    var img = document.getElementById("lightbox-img");
    var cap = document.getElementById("lightbox-cap");
    if (!img || !cap) return;

    var full = lang === "zh" ? trigger.dataset.fullZh : trigger.dataset.fullEn;
    var caption = lang === "zh" ? trigger.dataset.capZh : trigger.dataset.capEn;
    if (full) img.src = full;
    img.alt = caption || "";
    cap.textContent = caption || "";
    var original = document.getElementById("lightbox-original");
    if (original && full) original.href = full;
  }

  function applyLang(lang, options) {
    options = options || {};
    currentLang = lang === "zh" ? "zh" : "en";
    root.setAttribute("data-lang", currentLang);
    root.setAttribute("lang", currentLang === "zh" ? "zh-CN" : "en");

    langButtons.forEach(function (button) {
      button.setAttribute("aria-pressed", button.getAttribute("data-set-lang") === currentLang ? "true" : "false");
    });

    var meta = META[currentLang];
    document.title = meta.title;
    var description = document.querySelector('meta[name="description"]');
    if (description) description.setAttribute("content", meta.desc);

    Array.prototype.forEach.call(document.querySelectorAll("img[data-src-en][data-src-zh]"), function (img) {
      updateImage(img, currentLang);
    });

    Array.prototype.forEach.call(document.querySelectorAll("[data-aria-en][data-aria-zh]"), function (el) {
      el.setAttribute("aria-label", currentLang === "zh" ? el.dataset.ariaZh : el.dataset.ariaEn);
    });

    if (dialog && dialog.open) fillLightbox(activeTrigger, currentLang);

    if (status && options.announce) {
      status.textContent = currentLang === "zh" ? "界面语言：中文" : "Interface language: English";
    }
    if (options.persist) safeStorageSet("biwrite-site-lang", currentLang);
    if (options.updateUrl) setUrlLang(currentLang);
  }

  /* Language buttons ------------------------------------------------------- */
  langButtons.forEach(function (button) {
    button.addEventListener("click", function () {
      var lang = button.getAttribute("data-set-lang");
      if (lang === currentLang) return;
      applyLang(lang, { persist: true, updateUrl: true, announce: true });
    });
  });

  applyLang(currentLang, { persist: false, updateUrl: false, announce: false });

  /* Sticky header ---------------------------------------------------------- */
  var header = document.getElementById("site-header");
  if (header) {
    var setHeader = function () {
      header.classList.toggle("is-scrolled", window.scrollY > 8);
    };
    setHeader();
    window.addEventListener("scroll", setHeader, { passive: true });
  }

  /* Reveal on scroll ------------------------------------------------------- */
  root.classList.add("js");
  var reveals = Array.prototype.slice.call(document.querySelectorAll(".reveal"));
  if ("IntersectionObserver" in window) {
    var observer = new IntersectionObserver(
      function (entries) {
        entries.forEach(function (entry) {
          if (entry.isIntersecting) {
            entry.target.classList.add("is-visible");
            observer.unobserve(entry.target);
          }
        });
      },
      { rootMargin: "0px 0px -7% 0px", threshold: 0.06 }
    );
    reveals.forEach(function (el) {
      /* Reveal what is on screen right away; observe the rest. */
      var box = el.getBoundingClientRect();
      if (box.top < window.innerHeight * 0.93 && box.bottom > 0) {
        el.classList.add("is-visible");
      } else {
        observer.observe(el);
      }
    });
  } else {
    reveals.forEach(function (el) {
      el.classList.add("is-visible");
    });
  }

  /* Paired paragraphs ------------------------------------------------------ */
  Array.prototype.forEach.call(document.querySelectorAll(".spread"), function (spread) {
    var paras = Array.prototype.slice.call(spread.querySelectorAll("[data-pair]"));
    paras.forEach(function (para) {
      var key = para.getAttribute("data-pair");
      var setPaired = function (on) {
        paras.forEach(function (other) {
          if (other.getAttribute("data-pair") === key) {
            other.classList.toggle("is-paired", on);
          }
        });
      };
      para.addEventListener("mouseenter", function () {
        setPaired(true);
      });
      para.addEventListener("mouseleave", function () {
        setPaired(false);
      });
    });
  });

  /* Lightbox --------------------------------------------------------------- */
  if (dialog && typeof dialog.showModal === "function") {
    var closeButton = dialog.querySelector(".lightbox-close");

    Array.prototype.forEach.call(document.querySelectorAll("[data-lightbox]"), function (trigger) {
      trigger.addEventListener("click", function (event) {
        event.preventDefault();
        activeTrigger = trigger;
        fillLightbox(trigger, currentLang);
        dialog.showModal();
        document.body.classList.add("dialog-open");
      });
    });

    if (closeButton) {
      closeButton.addEventListener("click", function () {
        dialog.close();
      });
    }

    dialog.addEventListener("click", function (event) {
      if (event.target === dialog) dialog.close();
    });

    dialog.addEventListener("close", function () {
      document.body.classList.remove("dialog-open");
    });
  }
})();
