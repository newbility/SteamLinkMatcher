const state = {
  names: [],
  results: [],
  matching: false,
  activeIndex: -1,
  reviewOnly: false,
  opening: false,
};

const els = {
  nameInput: document.querySelector("#nameInput"),
  fileInput: document.querySelector("#fileInput"),
  importBtn: document.querySelector("#importBtn"),
  matchBtn: document.querySelector("#matchBtn"),
  confirmAllBtn: document.querySelector("#confirmAllBtn"),
  copyBtn: document.querySelector("#copyBtn"),
  copyBbcodeBtn: document.querySelector("#copyBbcodeBtn"),
  copySframeBtn: document.querySelector("#copySframeBtn"),
  exportCsvBtn: document.querySelector("#exportCsvBtn"),
  exportXlsxBtn: document.querySelector("#exportXlsxBtn"),
  clearSearchCacheBtn: document.querySelector("#clearSearchCacheBtn"),
  clearAllCacheBtn: document.querySelector("#clearAllCacheBtn"),
  openQueuedBtn: document.querySelector("#openQueuedBtn"),
  openScopeSelect: document.querySelector("#openScopeSelect"),
  openDelayInput: document.querySelector("#openDelayInput"),
  reviewOnlyToggle: document.querySelector("#reviewOnlyToggle"),
  proxyInput: document.querySelector("#proxyInput"),
  delayInput: document.querySelector("#delayInput"),
  saveProxyBtn: document.querySelector("#saveProxyBtn"),
  testProxyBtn: document.querySelector("#testProxyBtn"),
  proxyStatus: document.querySelector("#proxyStatus"),
  loadTextBtn: document.querySelector("#loadTextBtn"),
  clearTextBtn: document.querySelector("#clearTextBtn"),
  summary: document.querySelector("#summary"),
  progressText: document.querySelector("#progressText"),
  reviewText: document.querySelector("#reviewText"),
  progressBar: document.querySelector("#progressBar"),
  resultBody: document.querySelector("#resultBody"),
  modalBackdrop: document.querySelector("#modalBackdrop"),
  modalTitle: document.querySelector("#modalTitle"),
  modalSub: document.querySelector("#modalSub"),
  closeModalBtn: document.querySelector("#closeModalBtn"),
  candidateList: document.querySelector("#candidateList"),
  manualForm: document.querySelector("#manualForm"),
  manualUrl: document.querySelector("#manualUrl"),
  toast: document.querySelector("#toast"),
};

function parseTextarea() {
  return els.nameInput.value
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);
}

function setNames(names) {
  state.names = names;
  state.results = names.map((name) => ({
    original: name,
    cleaned: "",
    status: "等待匹配",
    steamTitle: "",
    appId: "",
    url: "",
    confidence: "",
    score: "",
    needsReview: false,
    source: "",
    candidates: [],
    message: "",
  }));
  render();
}

function showToast(message) {
  els.toast.textContent = message;
  els.toast.hidden = false;
  window.clearTimeout(showToast.timer);
  showToast.timer = window.setTimeout(() => {
    els.toast.hidden = true;
  }, 2400);
}

function statusClass(status) {
  if (status === "已匹配" || status === "已人工修正") return "ok";
  if (status === "未找到") return "missing";
  return "review";
}

function escapeHtml(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

function candidateCount(item) {
  return Array.isArray(item.candidates) ? item.candidates.length : 0;
}

function visibleResults() {
  if (!state.reviewOnly) {
    return state.results.map((item, index) => ({ item, index }));
  }
  return state.results
    .map((item, index) => ({ item, index }))
    .filter(({ item }) => item.needsReview);
}

function urlsForScope(scope) {
  return state.results
    .filter((item) => {
      if (!item.url) return false;
      if (scope === "review") return item.needsReview;
      if (scope === "matched") return !item.needsReview && item.status !== "未找到";
      return true;
    })
    .map((item) => item.url);
}

function render() {
  const total = state.results.length;
  const done = state.results.filter((item) => !["等待匹配", "匹配中"].includes(item.status)).length;
  const review = state.results.filter((item) => item.needsReview).length;
  const matched = state.results.filter((item) => item.url).length;
  const visible = visibleResults();
  els.summary.textContent = `${total} 条记录，${matched} 条有链接${state.reviewOnly ? `，正在显示 ${visible.length} 条需复核` : ""}`;
  els.reviewText.textContent = `需复核 ${review}`;
  els.progressText.textContent = state.matching ? `匹配中 ${done}/${total}` : total ? `已处理 ${done}/${total}` : "等待开始";
  els.progressBar.style.width = total ? `${Math.round((done / total) * 100)}%` : "0%";

  if (!total || !visible.length) {
    els.resultBody.innerHTML = `<tr class="empty-row"><td colspan="7">${total ? "没有符合筛选条件的记录" : "暂无记录"}</td></tr>`;
  } else {
    els.resultBody.innerHTML = visible
      .map(({ item, index }) => {
        const url = item.url
          ? `<a href="${escapeHtml(item.url)}" target="_blank" rel="noreferrer">${escapeHtml(item.url)}</a>`
          : escapeHtml(item.message || "");
        const score = item.score === "" || item.score === undefined ? "" : `${item.score}`;
        return `<tr>
          <td>${escapeHtml(item.original)}</td>
          <td>${escapeHtml(item.cleaned)}</td>
          <td><span class="pill ${statusClass(item.status)}">${escapeHtml(item.status)}</span></td>
          <td>${escapeHtml(item.steamTitle)}</td>
          <td class="link-cell">${url}</td>
          <td>${escapeHtml(score)}</td>
          <td>
            <div class="row-actions">
              ${item.needsReview && item.url ? `<button type="button" data-action="confirm" data-index="${index}">确认</button>` : ""}
              <button type="button" data-action="review" data-index="${index}">复核</button>
              ${item.url ? `<button type="button" data-action="open" data-index="${index}">打开</button>` : ""}
            </div>
          </td>
        </tr>`;
      })
      .join("");
  }

  const disabled = state.matching || state.opening;
  els.matchBtn.disabled = disabled;
  els.importBtn.disabled = disabled;
  els.loadTextBtn.disabled = disabled;
  els.copyBtn.disabled = disabled || !state.results.some((item) => item.url);
  els.copyBbcodeBtn.disabled = disabled || !state.results.some((item) => item.url && item.steamTitle);
  els.copySframeBtn.disabled = disabled || !state.results.some((item) => item.appId);
  els.confirmAllBtn.disabled = disabled || !state.results.some((item) => item.needsReview && item.url);
  els.openQueuedBtn.disabled = disabled || !urlsForScope(els.openScopeSelect.value).length;
  els.exportCsvBtn.disabled = disabled || !state.results.length;
  els.exportXlsxBtn.disabled = disabled || !state.results.length;
}

async function postJson(url, payload) {
  const response = await fetch(url, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
  });
  const data = await response.json();
  if (!response.ok) {
    throw new Error(data.error || "请求失败");
  }
  return data;
}

async function getJson(url) {
  const response = await fetch(url);
  const data = await response.json();
  if (!response.ok) {
    throw new Error(data.error || "请求失败");
  }
  return data;
}

function setProxyStatus(message) {
  els.proxyStatus.textContent = message;
}

async function loadSettings() {
  try {
    const settings = await getJson("/api/settings");
    els.proxyInput.value = settings.proxyUrl || "";
    els.delayInput.value = settings.requestDelayMs ?? 1500;
    setProxyStatus(settings.proxyUrl ? "已启用显式代理" : "未设置时使用当前系统网络");
  } catch (error) {
    setProxyStatus(error.message);
  }
}

async function saveProxy() {
  const proxyUrl = els.proxyInput.value.trim();
  const requestDelayMs = Number(els.delayInput.value || 0);
  const result = await postJson("/api/settings", { proxyUrl, requestDelayMs });
  const proxyText = result.proxyUrl ? "代理已启用" : "未设置代理";
  setProxyStatus(`${proxyText}，搜索间隔 ${result.requestDelayMs}ms`);
  showToast("Steam 访问设置已保存");
}

async function testProxy() {
  const proxyUrl = els.proxyInput.value.trim();
  setProxyStatus("正在测试 Steam 连接...");
  const result = await postJson("/api/proxy/test", { proxyUrl });
  setProxyStatus(result.message || (result.ok ? "代理测试成功" : "代理测试失败"));
  showToast(result.message || "测试完成");
}

async function startMatch() {
  const names = parseTextarea();
  if (!names.length) {
    showToast("没有可匹配的游戏名称");
    return;
  }
  setNames(names);
  state.matching = true;
  render();
  for (let index = 0; index < state.names.length; index += 1) {
    state.results[index] = { ...state.results[index], status: "匹配中" };
    render();
    try {
      const result = await postJson("/api/match", { name: state.names[index] });
      state.results[index] = result;
    } catch (error) {
      state.results[index] = {
        ...state.results[index],
        cleaned: state.names[index],
        status: "未找到",
        confidence: "低",
        needsReview: true,
        message: error.message,
      };
    }
    render();
  }
  state.matching = false;
  render();
  showToast("匹配完成");
}

async function importFile(file) {
  const raw = await file.arrayBuffer();
  const response = await fetch(`/api/import?filename=${encodeURIComponent(file.name)}`, {
    method: "POST",
    headers: { "Content-Type": "application/octet-stream" },
    body: raw,
  });
  const data = await response.json();
  if (!response.ok) {
    throw new Error(data.error || "导入失败");
  }
  els.nameInput.value = data.names.join("\n");
  setNames(data.names);
  showToast(`已导入 ${data.names.length} 条`);
}

function openReview(index) {
  state.activeIndex = index;
  const item = state.results[index];
  els.modalTitle.textContent = item.original || "复核候选";
  els.modalSub.textContent = `${item.cleaned || item.original} · ${item.status}`;
  els.manualUrl.value = item.url || "";
  const candidates = item.candidates || [];
  if (!candidates.length) {
    els.candidateList.innerHTML = '<div class="candidate"><div><strong>无候选</strong><span>可以手动粘贴 Steam 游戏本体链接</span></div></div>';
  } else {
    els.candidateList.innerHTML = candidates
      .map(
        (candidate, candidateIndex) => `<article class="candidate">
          <div>
            <strong>${escapeHtml(candidate.title)}</strong>
            <span>App ID ${escapeHtml(candidate.appId)} · 分数 ${escapeHtml(candidate.score)} · ${escapeHtml(candidate.url)}</span>
          </div>
          <button type="button" data-action="choose-candidate" data-candidate="${candidateIndex}">选择</button>
        </article>`,
      )
      .join("");
  }
  els.modalBackdrop.hidden = false;
}

function closeReview() {
  els.modalBackdrop.hidden = true;
  state.activeIndex = -1;
}

async function saveManual(payload) {
  const result = await postJson("/api/manual", payload);
  if (state.activeIndex >= 0) {
    state.results[state.activeIndex] = result;
    render();
  }
  closeReview();
  showToast("已保存修正");
}

async function confirmItem(index) {
  const item = state.results[index];
  if (!item || !item.url) return;
  const result = await postJson("/api/confirm", item);
  state.results[index] = result;
  render();
}

async function confirmAllReview() {
  const indexes = state.results
    .map((item, index) => (item.needsReview && item.url ? index : -1))
    .filter((index) => index >= 0);
  if (!indexes.length) {
    showToast("没有可确认的复核项");
    return;
  }
  if (!window.confirm(`确认 ${indexes.length} 条需复核结果？`)) return;
  for (const index of indexes) {
    await confirmItem(index);
  }
  showToast(`已确认 ${indexes.length} 条`);
}

async function chooseCandidate(candidateIndex) {
  const item = state.results[state.activeIndex];
  const candidate = item.candidates[candidateIndex];
  if (!candidate) return;
  await saveManual({
    original: item.original,
    url: candidate.url,
    appId: candidate.appId,
    steamTitle: candidate.title,
    candidates: item.candidates,
  });
}

async function copyLinks() {
  const links = state.results.map((item) => item.url).filter(Boolean).join("\n");
  if (!links) {
    showToast("没有可复制的链接");
    return;
  }
  await navigator.clipboard.writeText(links);
  showToast(`已复制 ${links.split("\n").length} 个链接`);
}

async function copyBbcode() {
  const rows = state.results
    .filter((item) => item.url && item.steamTitle)
    .map((item) => `[url=${item.url}]${item.steamTitle}[/url]`);
  if (!rows.length) {
    showToast("没有可复制的 BBCode");
    return;
  }
  await navigator.clipboard.writeText(rows.join("\n"));
  showToast(`已复制 ${rows.length} 条 BBCode`);
}

async function copySframe() {
  const rows = state.results
    .filter((item) => item.appId)
    .map((item) => `[sframe]${item.appId}[/sframe]`);
  if (!rows.length) {
    showToast("没有可复制的 sframe");
    return;
  }
  await navigator.clipboard.writeText(rows.join("\n"));
  showToast(`已复制 ${rows.length} 条 sframe`);
}

function sleep(ms) {
  return new Promise((resolve) => window.setTimeout(resolve, ms));
}

function openUrl(url) {
  window.open(url, "_blank", "noopener,noreferrer");
}

async function openQueuedLinks() {
  const scope = els.openScopeSelect.value;
  const urls = urlsForScope(scope);
  if (!urls.length) {
    showToast("没有可打开的链接");
    return;
  }
  if (!window.confirm(`将按队列打开 ${urls.length} 个链接。浏览器可能会拦截过多标签页，继续？`)) return;
  const delay = Math.max(100, Math.min(Number(els.openDelayInput.value || 500), 10000));
  state.opening = true;
  render();
  try {
    for (let index = 0; index < urls.length; index += 1) {
      openUrl(urls[index]);
      showToast(`正在打开 ${index + 1}/${urls.length}`);
      if (index < urls.length - 1) {
        await sleep(delay);
      }
    }
    showToast(`已尝试打开 ${urls.length} 个链接`);
  } finally {
    state.opening = false;
    render();
  }
}

async function downloadExport(kind) {
  const response = await fetch(`/api/export/${kind}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ results: state.results }),
  });
  if (!response.ok) {
    const data = await response.json().catch(() => ({}));
    throw new Error(data.error || "导出失败");
  }
  const blob = await response.blob();
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = kind === "xlsx" ? "steam_links_result.xlsx" : "steam_links_result.csv";
  document.body.appendChild(link);
  link.click();
  link.remove();
  URL.revokeObjectURL(url);
}

els.importBtn.addEventListener("click", () => els.fileInput.click());
els.fileInput.addEventListener("change", async () => {
  const file = els.fileInput.files[0];
  if (!file) return;
  try {
    await importFile(file);
  } catch (error) {
    showToast(error.message);
  } finally {
    els.fileInput.value = "";
  }
});

els.loadTextBtn.addEventListener("click", () => {
  const names = parseTextarea();
  setNames(names);
  showToast(`已载入 ${names.length} 条`);
});

els.clearTextBtn.addEventListener("click", () => {
  els.nameInput.value = "";
  setNames([]);
});

els.matchBtn.addEventListener("click", startMatch);
els.confirmAllBtn.addEventListener("click", () => confirmAllReview().catch((error) => showToast(error.message)));
els.copyBtn.addEventListener("click", () => copyLinks().catch((error) => showToast(error.message)));
els.copyBbcodeBtn.addEventListener("click", () => copyBbcode().catch((error) => showToast(error.message)));
els.copySframeBtn.addEventListener("click", () => copySframe().catch((error) => showToast(error.message)));
els.openQueuedBtn.addEventListener("click", () => openQueuedLinks().catch((error) => showToast(error.message)));
els.openScopeSelect.addEventListener("change", render);
els.reviewOnlyToggle.addEventListener("change", () => {
  state.reviewOnly = els.reviewOnlyToggle.checked;
  render();
});
els.exportCsvBtn.addEventListener("click", () => downloadExport("csv").catch((error) => showToast(error.message)));
els.exportXlsxBtn.addEventListener("click", () => downloadExport("xlsx").catch((error) => showToast(error.message)));
els.saveProxyBtn.addEventListener("click", () => saveProxy().catch((error) => {
  setProxyStatus(error.message);
  showToast(error.message);
}));
els.testProxyBtn.addEventListener("click", () => testProxy().catch((error) => {
  setProxyStatus(error.message);
  showToast(error.message);
}));
async function clearCache(mode) {
  const isAll = mode === "all";
  const message = isAll
    ? "清空所有缓存？这会删除搜索缓存以及人工确认/修正记录。"
    : "清空搜索缓存？人工确认/修正记录会保留。";
  if (!window.confirm(message)) return;
  try {
    await postJson("/api/cache/clear", { mode });
    showToast(isAll ? "全部缓存已清空" : "搜索缓存已清空，人工记录已保留");
  } catch (error) {
    showToast(error.message);
  }
}

els.clearSearchCacheBtn.addEventListener("click", () => clearCache("search"));
els.clearAllCacheBtn.addEventListener("click", () => clearCache("all"));

els.resultBody.addEventListener("click", (event) => {
  const button = event.target.closest("button");
  if (!button) return;
  const action = button.dataset.action;
  const index = Number(button.dataset.index);
  if (action === "confirm") confirmItem(index).then(() => showToast("已确认")).catch((error) => showToast(error.message));
  if (action === "review") openReview(index);
  if (action === "open") window.open(state.results[index].url, "_blank", "noopener,noreferrer");
});

els.candidateList.addEventListener("click", (event) => {
  const button = event.target.closest("button");
  if (!button || button.dataset.action !== "choose-candidate") return;
  chooseCandidate(Number(button.dataset.candidate)).catch((error) => showToast(error.message));
});

els.manualForm.addEventListener("submit", (event) => {
  event.preventDefault();
  const item = state.results[state.activeIndex];
  const url = els.manualUrl.value.trim();
  if (!url) {
    showToast("请输入 Steam 链接");
    return;
  }
  saveManual({
    original: item.original,
    url,
    candidates: item.candidates,
    source: "手动粘贴",
  }).catch((error) => showToast(error.message));
});

els.closeModalBtn.addEventListener("click", closeReview);
els.modalBackdrop.addEventListener("click", (event) => {
  if (event.target === els.modalBackdrop) closeReview();
});

window.addEventListener("keydown", (event) => {
  if (event.key === "Escape" && !els.modalBackdrop.hidden) closeReview();
});

render();
loadSettings();
