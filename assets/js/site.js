/* BiWrite official site — language switch, 4-mode showcase, lightbox, copy & release sync */
(function () {
  "use strict";

  var root = document.documentElement;
  var langButtons = Array.prototype.slice.call(document.querySelectorAll("[data-set-lang]"));
  var status = document.getElementById("lang-status");
  var dialog = document.getElementById("lightbox");
  var currentLang = root.getAttribute("data-lang") === "en" ? "en" : "zh";
  var currentTab = "editor";

  var META = {
    zh: {
      title: "BiWrite — 专注学术论文的中英双语桌面编辑器 (LaTeX & Markdown)",
      desc: "BiWrite 是一款开源的中英双语学术论文桌面编辑器。支持段落级增量重译、LaTeX 公式与引用占位符无损保护、本地不保存隔离编译、PDF 句子级双向定位、paper_zh.tex 双文件同步，以及内置 research-builder 规范的去 AI 腔润色与科研绘图助手。"
    },
    en: {
      title: "BiWrite — Bilingual Desktop Editor for Academic Papers (LaTeX & Markdown)",
      desc: "Open-source desktop editor for writing academic papers in English and Chinese at once. Incremental paragraph translation, byte-exact LaTeX placeholder protection, local SyncTeX compilation, and a research-builder writing assistant."
    }
  };

  var SCENES = {
    editor: {
      windowTitle: "paper.tex — BiWrite",
      zh: {
        src: "assets/screens/editor-zh-1700.webp",
        srcset: "assets/screens/editor-zh-900.webp 900w, assets/screens/editor-zh-1700.webp 1700w",
        alt: "BiWrite 编辑器界面：左侧编写英文 LaTeX 源码，右侧逐段实时显示中文译文",
        cap: "双语逐段对照：左侧编写 paper.tex，右侧按段落一一对应显示中文。仅第 8 段被修改，因此只有第 8 段基于原译文增量修订，其余段落直接命中缓存跳过。",
        html: "<strong>双语逐段对照：</strong>左侧编写 <code>paper.tex</code>，右侧按段落一一对应显示中文。图中仅第 8 段被修改，因此只有第 8 段基于原译文增量修订，其余段落直接命中缓存跳过（底部状态栏实时统计已翻译与跳过段数、请求耗时及缓存命中）。"
      },
      en: {
        src: "assets/screens/editor-en-1700.webp",
        srcset: "assets/screens/editor-en-900.webp 900w, assets/screens/editor-en-1700.webp 1700w",
        alt: "BiWrite editor: English LaTeX manuscript on the left and aligned Chinese paragraphs on the right",
        cap: "Incremental Bilingual Editing: Edit paper.tex on the left while aligned translations appear block by block on the right. Only paragraph 8 was modified, so only paragraph 8 is revised.",
        html: "<strong>Incremental Bilingual Editing:</strong> Edit <code>paper.tex</code> on the left while aligned translations appear block by block on the right. Only paragraph 8 was modified, so only paragraph 8 is revised from its prior translation while unchanged blocks stay untouched."
      }
    },
    pdf: {
      windowTitle: "paper.tex (PDF & SyncTeX) — BiWrite",
      zh: {
        src: "assets/screens/pdf-zh-1700.webp",
        srcset: "assets/screens/pdf-zh-900.webp 900w, assets/screens/pdf-zh-1700.webp 1700w",
        alt: "BiWrite 本地 LaTeX 编译预览与点击 PDF 选中源码句子界面",
        cap: "本地编译与点击定位：未保存草稿在隐藏的 .biwrite/ 缓存内直接编译；点击 PDF 任意句子自动选中左侧对应源码，并浮出快捷菜单直接润色或提问。",
        html: "<strong>本地编译与点击定位：</strong>无需先保存磁盘文件，即可在隐藏的 <code>.biwrite/</code> 缓存目录编译预览中英文 PDF。点击 PDF 里的任意一句话即在左侧自动选中对应源码，并弹出「润色 / 提问」快捷入口；上方问题列表点击直达报错行。"
      },
      en: {
        src: "assets/screens/pdf-en-1700.webp",
        srcset: "assets/screens/pdf-en-900.webp 900w, assets/screens/pdf-en-1700.webp 1700w",
        alt: "BiWrite compiling LaTeX locally with click-to-source SyncTeX selection and inline actions",
        cap: "Local Build & Click-to-Source: Unsaved edits compile inside a hidden .biwrite/ cache; clicking any sentence in the PDF selects its exact LaTeX source and offers quick Polish or Ask actions.",
        html: "<strong>Local Build &amp; Click-to-Source:</strong> Unsaved edits compile inside a hidden <code>.biwrite/</code> cache without dirtying files on disk. Click any sentence in the PDF to select its exact LaTeX source and launch quick <em>Polish</em> or <em>Ask</em> actions; errors link straight to their lines."
      }
    },
    assistant: {
      windowTitle: "paper.tex (Writing Assistant) — BiWrite",
      zh: {
        src: "assets/screens/assistant-zh-1700.webp",
        srcset: "assets/screens/assistant-zh-900.webp 900w, assets/screens/assistant-zh-1700.webp 1700w",
        alt: "BiWrite 写作助手：遵循 research-builder 规范的润色 Diff 预览、对照译文与改动说明",
        cap: "受控学术写作助手：遵循 research-builder 去 AI 腔与文风规则，所有结果以逐词 Diff、对照中文与中英双语理由呈现；点「采纳」前绝不擅自改动正文。",
        html: "<strong>受控学术写作助手：</strong>支持去 AI 腔润色、提问、参照范文仿写、生成可编辑 PPT / TikZ 图表与语法修复。后台运行不阻塞打字，返回结果以逐词 Diff、对照中文与双语修改理由呈现；点「采纳」时会先在后台试编译检查，通过后才写入编辑器。"
      },
      en: {
        src: "assets/screens/assistant-en-1700.webp",
        srcset: "assets/screens/assistant-en-900.webp 900w, assets/screens/assistant-en-1700.webp 1700w",
        alt: "BiWrite writing assistant showing inline diff, facing translation, and bilingual edit rationale",
        cap: "Rule-Based Assistant: Follows research-builder rules for de-AI polishing, imitation, figures, and TeX repair. Results arrive as inline diffs and test-compile before anything enters your draft.",
        html: "<strong>Rule-Based Academic Assistant:</strong> Polish, ask, imitate reference papers, generate editable PPTX or TikZ figures, and repair TeX errors in the background. Results arrive with inline diffs, facing translations, and rationale — test-compiled before <em>Accept</em> writes to your buffer."
      }
    },
    pair: {
      windowTitle: "paper.tex ⇄ paper_zh.tex — BiWrite",
      zh: {
        src: "assets/screens/pair-zh-1700.webp",
        srcset: "assets/screens/pair-zh-900.webp 900w, assets/screens/pair-zh-1700.webp 1700w",
        alt: "BiWrite 与已有 paper_zh.tex 逐段自动配对，并通过改写中文译文同步微调英文原稿",
        cap: "双文件配对与按译文改：自动对齐 paper.tex 与 paper_zh.tex（21 段全部对上，零翻译请求）；在右侧「按译文改」直接修改中文，英文原段即以最小改动同步语义。",
        html: "<strong>双文件配对与「按译文改」：</strong>打开 <code>paper.tex</code> 自动识别并对齐手写的 <code>paper_zh.tex</code>（底部显示 21 段全部对应，零 API 请求）。右侧打开「按译文改」直接改写第 8 段中文，即可让英文原稿以最小编辑幅度同步更新，保存时双文件原位回写。"
      },
      en: {
        src: "assets/screens/pair-en-1700.webp",
        srcset: "assets/screens/pair-en-900.webp 900w, assets/screens/pair-en-1700.webp 1700w",
        alt: "BiWrite pairing paper.tex with an existing paper_zh.tex and editing English via its Chinese translation",
        cap: "Existing File Pairing & Edit via Translation: Automatically pairs 21/21 paragraphs with paper_zh.tex using zero API calls, while 'Via translation' updates the English paragraph with minimal edits when you rewrite the Chinese.",
        html: "<strong>Existing File Pairing &amp; Edit via Translation:</strong> Automatically aligns <code>paper.tex</code> with your existing <code>paper_zh.tex</code> (21 of 21 paragraphs paired, zero API requests). Use <em>Via translation</em> to rewrite a Chinese block directly and let the English follow with minimal edits."
      }
    }
  };

  function safeSetStorage(key, val) {
    try { localStorage.setItem(key, val); } catch (e) {}
  }

  function setUrlLang(lang) {
    if (!/^https?:$/.test(location.protocol)) return;
    try {
      var url = new URL(location.href);
      if (url.searchParams.get("lang") === lang) return;
      url.searchParams.set("lang", lang);
      history.replaceState(null, "", url.toString());
    } catch (e) {}
  }

  function renderShowcase() {
    var scene = SCENES[currentTab] || SCENES.editor;
    var loc = scene[currentLang] || scene.zh;
    var img = document.getElementById("showcase-img");
    var titleEl = document.getElementById("showcase-filename");
    var descZh = document.getElementById("showcase-desc-zh");
    var descEn = document.getElementById("showcase-desc-en");
    var panel = document.getElementById("showcase-panel");

    if (titleEl) titleEl.textContent = scene.windowTitle;
    if (img) {
      img.setAttribute("srcset", loc.srcset);
      img.setAttribute("src", loc.src);
      img.setAttribute("alt", loc.alt);
    }
    if (descZh) descZh.innerHTML = scene.zh.html;
    if (descEn) descEn.innerHTML = scene.en.html;
    if (panel) panel.setAttribute("aria-labelledby", "tab-" + currentTab);

    if (dialog && dialog.open) {
      fillLightbox(loc.src, loc.cap);
    }
  }

  function fillLightbox(src, caption) {
    var img = document.getElementById("lightbox-img");
    var cap = document.getElementById("lightbox-cap");
    var raw = document.getElementById("lightbox-original");
    if (img) {
      img.src = src;
      img.alt = caption || "";
    }
    if (cap) cap.textContent = caption || "";
    if (raw) raw.href = src;
  }

  function applyLang(lang, opts) {
    opts = opts || {};
    currentLang = lang === "en" ? "en" : "zh";
    root.setAttribute("data-lang", currentLang);
    root.setAttribute("lang", currentLang === "zh" ? "zh-CN" : "en");

    langButtons.forEach(function (btn) {
      btn.setAttribute("aria-pressed", btn.getAttribute("data-set-lang") === currentLang ? "true" : "false");
    });

    var meta = META[currentLang];
    document.title = meta.title;
    var descMeta = document.querySelector('meta[name="description"]');
    if (descMeta) descMeta.setAttribute("content", meta.desc);

    Array.prototype.forEach.call(document.querySelectorAll("[data-manual-link]"), function (link) {
      var href = currentLang === "zh" ? link.getAttribute("data-href-zh") : link.getAttribute("data-href-en");
      if (href) link.setAttribute("href", href);
    });

    renderShowcase();

    if (status && opts.announce) {
      status.textContent = currentLang === "zh" ? "已切换为中文界面" : "Switched to English";
    }
    if (opts.persist) safeSetStorage("biwrite-site-lang", currentLang);
    if (opts.updateUrl) setUrlLang(currentLang);
  }

  /* Language buttons */
  langButtons.forEach(function (btn) {
    btn.addEventListener("click", function () {
      var next = btn.getAttribute("data-set-lang");
      if (next === currentLang) return;
      applyLang(next, { persist: true, updateUrl: true, announce: true });
    });
  });

  /* Showcase tabs */
  var tabButtons = Array.prototype.slice.call(document.querySelectorAll(".showcase-tab[data-tab]"));
  function activateTab(tabKey, focusTab) {
    if (!SCENES[tabKey]) return;
    currentTab = tabKey;
    tabButtons.forEach(function (btn) {
      var active = btn.getAttribute("data-tab") === tabKey;
      btn.classList.toggle("is-active", active);
      btn.setAttribute("aria-selected", active ? "true" : "false");
      if (active && focusTab) btn.focus();
    });
    renderShowcase();
  }

  tabButtons.forEach(function (btn, idx) {
    btn.addEventListener("click", function () {
      activateTab(btn.getAttribute("data-tab"), false);
    });
    btn.addEventListener("keydown", function (ev) {
      if (ev.key !== "ArrowRight" && ev.key !== "ArrowLeft") return;
      ev.preventDefault();
      var dir = ev.key === "ArrowRight" ? 1 : -1;
      var nextIdx = (idx + dir + tabButtons.length) % tabButtons.length;
      activateTab(tabButtons[nextIdx].getAttribute("data-tab"), true);
    });
  });

  applyLang(currentLang, { persist: false, updateUrl: false, announce: false });

  /* Sticky header shadow */
  var header = document.getElementById("site-header");
  if (header) {
    var syncHeader = function () {
      header.classList.toggle("is-scrolled", window.scrollY > 8);
    };
    syncHeader();
    window.addEventListener("scroll", syncHeader, { passive: true });
  }

  /* Lightbox */
  if (dialog && typeof dialog.showModal === "function") {
    var closeBtn = dialog.querySelector(".lightbox-close");
    Array.prototype.forEach.call(document.querySelectorAll("[data-lightbox-current]"), function (trigger) {
      trigger.addEventListener("click", function (ev) {
        ev.preventDefault();
        var scene = SCENES[currentTab] || SCENES.editor;
        var loc = scene[currentLang] || scene.zh;
        fillLightbox(loc.src, loc.cap);
        dialog.showModal();
        document.body.classList.add("dialog-open");
      });
    });

    if (closeBtn) {
      closeBtn.addEventListener("click", function () { dialog.close(); });
    }
    dialog.addEventListener("click", function (ev) {
      if (ev.target === dialog) dialog.close();
    });
    dialog.addEventListener("close", function () {
      document.body.classList.remove("dialog-open");
    });
  }

  /* Copy terminal command */
  Array.prototype.forEach.call(document.querySelectorAll("[data-copy-target]"), function (btn) {
    btn.addEventListener("click", function () {
      var targetId = btn.getAttribute("data-copy-target");
      var el = targetId ? document.getElementById(targetId) : null;
      if (!el || !navigator.clipboard) return;
      var text = el.textContent.trim();
      navigator.clipboard.writeText(text).then(function () {
        btn.classList.add("is-copied");
        var label = btn.querySelector(".copy-label");
        if (label) {
          label.textContent = currentLang === "zh" ? "已复制" : "Copied";
          setTimeout(function () {
            btn.classList.remove("is-copied");
            label.innerHTML = '<span class="lang-zh">复制</span><span class="lang-en">Copy</span>';
          }, 1800);
        }
      }).catch(function () {});
    });
  });

  /* Subtle highlight of visitor's current OS card */
  try {
    var ua = (navigator.userAgent || "").toLowerCase();
    var osKey = ua.indexOf("mac") !== -1 ? "macos" : (ua.indexOf("win") !== -1 ? "windows" : "");
    if (osKey) {
      var card = document.querySelector('.os-card[data-dl-os="' + osKey + '"]');
      if (card) card.classList.add("is-recommended");
    }
  } catch (e) {}

  /* Live GitHub release sync (gracefully enhances version badge & direct installer URLs) */
  if (typeof fetch === "function" && /^https?:$/.test(location.protocol)) {
    fetch("https://api.github.com/repos/zzccppp/biwrite/releases/latest", {
      headers: { Accept: "application/vnd.github+json" }
    })
      .then(function (res) { return res.ok ? res.json() : null; })
      .then(function (data) {
        if (!data || !data.tag_name) return;
        var tag = data.tag_name;
        Array.prototype.forEach.call(document.querySelectorAll("[data-release-version]"), function (el) {
          el.textContent = tag;
        });
        var assets = Array.isArray(data.assets) ? data.assets : [];
        assets.forEach(function (asset) {
          if (!asset || !asset.name || !asset.browser_download_url) return;
          if (/\.dmg$/i.test(asset.name)) {
            var macLink = document.querySelector('.os-card[data-dl-os="macos"]');
            if (macLink) macLink.setAttribute("href", asset.browser_download_url);
            Array.prototype.forEach.call(document.querySelectorAll("[data-dmg-name]"), function (el) {
              el.textContent = asset.name;
            });
          } else if (/\.exe$/i.test(asset.name)) {
            var winLink = document.querySelector('.os-card[data-dl-os="windows"]');
            if (winLink) winLink.setAttribute("href", asset.browser_download_url);
            Array.prototype.forEach.call(document.querySelectorAll("[data-exe-name]"), function (el) {
              el.textContent = asset.name;
            });
          }
        });
      })
      .catch(function () { /* keep static fallback */ });
  }
})();
