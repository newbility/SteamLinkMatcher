<script lang="ts">
  import { invoke } from '@tauri-apps/api/core'
  import { confirm as confirmDialog, open, save } from '@tauri-apps/plugin-dialog'

  type Candidate = {
    appId: string
    title: string
    url: string
    score: number
    confidence: string
    type: string
  }

  type MatchResult = {
    original: string
    cleaned: string
    status: string
    steamTitle: string
    appId: string
    url: string
    confidence: string
    score: number
    needsReview: boolean
    source: string
    candidates: Candidate[]
    message: string
    fromCache: boolean
  }

  let nameInput = ''
  let results: MatchResult[] = []
  let matching = false
  let matchingPaused = false
  let opening = false
  let reviewOnly = false
  let progressText = '等待开始'
  let proxyUrl = ''
  let requestDelayMs = 1500
  let proxyStatus = '未设置时使用当前系统网络'
  let proxyTesting = false
  let openScope = 'all'
  let openDelayMs = 500
  let toast = ''
  let modalOpen = false
  let activeIndex = -1
  let manualUrl = ''
  let tableScrollTop = 0
  let tableViewportHeight = 360
  const rowHeight = 76
  const overscanRows = 8

  $: total = results.length
  $: matched = results.filter((item) => item.url).length
  $: reviewCount = results.filter((item) => item.needsReview).length
  $: done = results.filter((item) => !['等待匹配', '匹配中'].includes(item.status)).length
  $: visibleResults = reviewOnly
    ? results.map((item, index) => ({ item, index })).filter(({ item }) => item.needsReview)
    : results.map((item, index) => ({ item, index }))
  $: rawVirtualStart = Math.max(0, Math.floor(tableScrollTop / rowHeight) - overscanRows)
  $: virtualStart = Math.min(rawVirtualStart, Math.max(0, visibleResults.length - 1))
  $: virtualEnd = Math.min(
    visibleResults.length,
    Math.max(virtualStart + 1, Math.ceil((tableScrollTop + tableViewportHeight) / rowHeight) + overscanRows),
  )
  $: virtualResults = visibleResults.slice(virtualStart, virtualEnd)
  $: topSpacerHeight = virtualStart * rowHeight
  $: bottomSpacerHeight = Math.max(0, (visibleResults.length - virtualEnd) * rowHeight)
  $: progressWidth = total ? `${Math.round((done / total) * 100)}%` : '0%'
  $: summary = `${total} 条记录，${matched} 条有链接${reviewOnly ? `，正在显示 ${visibleResults.length} 条需复核` : ''}`
  $: current = activeIndex >= 0 ? results[activeIndex] : null
  $: canOpen = urlsForScope(openScope).length > 0 && !matching && !opening

  function showToast(message: string) {
    toast = message
    window.clearTimeout((showToast as any).timer)
    ;(showToast as any).timer = window.setTimeout(() => {
      toast = ''
    }, 2400)
  }

  async function askConfirm(message: string, title = '请确认') {
    try {
      return await confirmDialog(message, {
        title,
        kind: 'warning',
        okLabel: '确认',
        cancelLabel: '取消',
      })
    } catch {
      return window.confirm(message)
    }
  }

  function parseTextarea() {
    return nameInput
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter(Boolean)
  }

  function setNames(names: string[]) {
    results = names.map((name) => ({
      original: name,
      cleaned: '',
      status: '等待匹配',
      steamTitle: '',
      appId: '',
      url: '',
      confidence: '',
      score: 0,
      needsReview: false,
      source: '',
      candidates: [],
      message: '',
      fromCache: false,
    }))
  }

  function statusClass(status: string) {
    if (status === '已匹配' || status === '已人工修正') return 'ok'
    if (status === '未找到') return 'missing'
    return 'review'
  }

  function shortSteamUrl(item: MatchResult) {
    if (item.appId) return `store.steampowered.com/app/${item.appId}`
    return item.url.replace(/^https?:\/\//, '').replace(/\/$/, '')
  }

  function handleTableScroll(event: Event) {
    tableScrollTop = (event.currentTarget as HTMLDivElement).scrollTop
  }

  async function loadSettings() {
    try {
      const settings = await invoke<{ proxyUrl: string; requestDelayMs: number; dataDir: string }>('get_settings')
      proxyUrl = settings.proxyUrl || ''
      requestDelayMs = settings.requestDelayMs || 1500
      proxyStatus = proxyUrl ? `已启用显式代理 · 数据目录 ${settings.dataDir}` : `未设置代理 · 数据目录 ${settings.dataDir}`
    } catch (error) {
      proxyStatus = String(error)
    }
  }

  async function saveSettings() {
    try {
      const settings = await invoke<{ proxyUrl: string; requestDelayMs: number }>('save_settings', {
        req: { proxyUrl, requestDelayMs },
      })
      proxyStatus = `${settings.proxyUrl ? '代理已启用' : '未设置代理'}，搜索间隔 ${settings.requestDelayMs}ms`
      showToast('设置已保存')
    } catch (error) {
      showToast(String(error))
    }
  }

  async function testProxy() {
    proxyTesting = true
    proxyStatus = '正在测试 Steam 连接...'
    try {
      const result = await invoke<{ ok: boolean; message: string }>('test_proxy', {
        req: { proxyUrl },
      })
      proxyStatus = result.message
      showToast(result.message)
    } catch (error) {
      proxyStatus = String(error)
      showToast(String(error))
    } finally {
      proxyTesting = false
    }
  }

  async function chooseImportFile() {
    const selected = await open({
      multiple: false,
      filters: [{ name: 'Game lists', extensions: ['txt', 'csv', 'xlsx'] }],
    })
    if (!selected || Array.isArray(selected)) return
    try {
      const names = await invoke<string[]>('import_file', { path: selected })
      nameInput = names.join('\n')
      setNames(names)
      showToast(`已导入 ${names.length} 条`)
    } catch (error) {
      showToast(String(error))
    }
  }

  async function startMatch() {
    const names = parseTextarea()
    if (!names.length) {
      showToast('没有可匹配的游戏名称')
      return
    }
    setNames(names)
    matching = true
    matchingPaused = false
    const working = [...results]
    let lastFlush = 0
    let pendingProgressText = progressText
    const flushResults = (force = false) => {
      const now = Date.now()
      if (force || now - lastFlush >= 200) {
        progressText = pendingProgressText
        results = [...working]
        lastFlush = now
      }
    }
    const waitWhilePaused = async (index: number) => {
      while (matchingPaused) {
        pendingProgressText = `已暂停 ${index}/${names.length}`
        flushResults(true)
        await sleep(120)
      }
    }
    for (let index = 0; index < names.length; index += 1) {
      await waitWhilePaused(index)
      working[index] = { ...working[index], status: '匹配中' }
      pendingProgressText = `匹配中 ${index}/${names.length}`
      flushResults()
      try {
        const result = await invoke<MatchResult>('match_game', { req: { name: names[index], force: false } })
        working[index] = result
      } catch (error) {
        working[index] = {
          ...working[index],
          cleaned: names[index],
          status: '未找到',
          confidence: '低',
          needsReview: true,
          message: String(error),
        }
      }
      flushResults()
      await waitWhilePaused(index + 1)
    }
    pendingProgressText = `已处理 ${names.length}/${names.length}`
    flushResults(true)
    results = [...working]
    matching = false
    matchingPaused = false
    showToast('匹配完成')
  }

  function toggleMatchPause() {
    if (!matching) return
    matchingPaused = !matchingPaused
    progressText = matchingPaused ? '暂停中，当前请求完成后停止继续匹配' : '继续匹配中'
    showToast(matchingPaused ? '已暂停匹配' : '已恢复匹配')
  }

  function openReview(index: number) {
    activeIndex = index
    manualUrl = results[index]?.url || ''
    modalOpen = true
  }

  async function openExternalUrl(url: string) {
    try {
      await invoke('open_external_url', { url })
    } catch (error) {
      showToast(String(error))
    }
  }

  function closeReview() {
    modalOpen = false
    activeIndex = -1
    manualUrl = ''
  }

  async function chooseCandidate(candidate: Candidate) {
    if (!current) return
    try {
      const result = await invoke<MatchResult>('manual_update', {
        req: {
          original: current.original,
          url: candidate.url,
          steamTitle: candidate.title,
          candidates: current.candidates,
          source: '人工修正',
        },
      })
      results[activeIndex] = result
      results = [...results]
      closeReview()
      showToast('已保存修正')
    } catch (error) {
      showToast(String(error))
    }
  }

  async function saveManual() {
    if (!current || !manualUrl.trim()) {
      showToast('请输入 Steam 链接')
      return
    }
    try {
      const result = await invoke<MatchResult>('manual_update', {
        req: {
          original: current.original,
          url: manualUrl.trim(),
          candidates: current.candidates,
          source: '手动粘贴',
        },
      })
      results[activeIndex] = result
      results = [...results]
      closeReview()
      showToast('已保存修正')
    } catch (error) {
      showToast(String(error))
    }
  }

  async function confirmItem(index: number) {
    try {
      const result = await invoke<MatchResult>('confirm_result', { result: results[index] })
      results[index] = result
      results = [...results]
    } catch (error) {
      showToast(String(error))
    }
  }

  async function confirmAllReview() {
    const indexes = results.map((item, index) => (item.needsReview && item.url ? index : -1)).filter((index) => index >= 0)
    if (!indexes.length) {
      showToast('没有可确认的复核项')
      return
    }
    if (!(await askConfirm(`确认 ${indexes.length} 条需复核结果？`, '确认复核结果'))) return
    for (const index of indexes) {
      await confirmItem(index)
    }
    showToast(`已确认 ${indexes.length} 条`)
  }

  async function copyText(text: string, emptyMessage: string, successMessage: string) {
    if (!text) {
      showToast(emptyMessage)
      return
    }
    await navigator.clipboard.writeText(text)
    showToast(successMessage)
  }

  function copyLinks() {
    const rows = results.map((item) => item.url).filter(Boolean)
    copyText(rows.join('\n'), '没有可复制的链接', `已复制 ${rows.length} 个链接`)
  }

  function copyBbcode() {
    const rows = results.filter((item) => item.url && item.steamTitle).map((item) => `[url=${item.url}]${item.steamTitle}[/url]`)
    copyText(rows.join('\n'), '没有可复制的 BBCode', `已复制 ${rows.length} 条 BBCode`)
  }

  function copySframe() {
    const rows = results.filter((item) => item.appId).map((item) => `[sframe]${item.appId}[/sframe]`)
    copyText(rows.join('\n'), '没有可复制的 sframe', `已复制 ${rows.length} 条 sframe`)
  }

  function urlsForScope(scope: string) {
    return results
      .filter((item) => {
        if (!item.url) return false
        if (scope === 'review') return item.needsReview
        if (scope === 'matched') return !item.needsReview && item.status !== '未找到'
        return true
      })
      .map((item) => item.url)
  }

  function sleep(ms: number) {
    return new Promise((resolve) => window.setTimeout(resolve, ms))
  }

  async function openQueuedLinks() {
    const urls = urlsForScope(openScope)
    if (!urls.length) {
      showToast('没有可打开的链接')
      return
    }
    if (!(await askConfirm(`将按队列打开 ${urls.length} 个链接，继续？`, '打开链接'))) return
    opening = true
    try {
      const delay = Math.max(100, Math.min(Number(openDelayMs || 500), 10000))
      for (let index = 0; index < urls.length; index += 1) {
        await invoke('open_external_url', { url: urls[index] })
        showToast(`正在打开 ${index + 1}/${urls.length}`)
        if (index < urls.length - 1) await sleep(delay)
      }
      showToast(`已尝试打开 ${urls.length} 个链接`)
    } finally {
      opening = false
    }
  }

  async function exportResults(kind: 'csv' | 'xlsx') {
    const path = await save({
      defaultPath: kind === 'xlsx' ? 'steam_links_result.xlsx' : 'steam_links_result.csv',
      filters: [{ name: kind.toUpperCase(), extensions: [kind] }],
    })
    if (!path) return
    try {
      await invoke('export_file', { req: { path, kind, results } })
      showToast('导出完成')
    } catch (error) {
      showToast(String(error))
    }
  }

  async function clearCache(mode: 'search' | 'all') {
    const isAll = mode === 'all'
    const message = isAll
      ? '清空所有缓存？这会删除搜索缓存以及人工确认/修正记录。'
      : '清空搜索缓存？人工确认/修正记录会保留。'
    if (!(await askConfirm(message, isAll ? '清空所有缓存' : '清空搜索缓存'))) return
    try {
      await invoke('clear_cache', { req: { mode } })
      showToast(isAll ? '全部缓存已清空' : '搜索缓存已清空，人工记录已保留')
    } catch (error) {
      showToast(String(error))
    }
  }

  loadSettings()
</script>

<main class="shell">
  <section class="topbar">
    <div>
      <h1>Steam 链接匹配</h1>
      <p>{summary}</p>
    </div>
    <div class="actions">
      <button type="button" on:click={chooseImportFile} disabled={matching || opening}>导入</button>
      <button type="button" class="primary" on:click={startMatch} disabled={matching || opening}>开始匹配</button>
      <button type="button" on:click={toggleMatchPause} disabled={!matching || opening}>{matchingPaused ? '继续匹配' : '暂停匹配'}</button>
      <button type="button" on:click={confirmAllReview} disabled={matching || opening || !results.some((item) => item.needsReview && item.url)}>确认全部复核</button>
      <button type="button" on:click={copyLinks} disabled={!results.some((item) => item.url)}>复制链接</button>
      <button type="button" on:click={copyBbcode} disabled={!results.some((item) => item.url && item.steamTitle)}>复制 BBCode</button>
      <button type="button" on:click={copySframe} disabled={!results.some((item) => item.appId)}>复制 sframe</button>
      <button type="button" on:click={() => exportResults('csv')} disabled={!results.length}>CSV</button>
      <button type="button" on:click={() => exportResults('xlsx')} disabled={!results.length}>XLSX</button>
      <button type="button" class="ghost" on:click={() => clearCache('search')}>清搜索缓存</button>
      <button type="button" class="ghost" on:click={() => clearCache('all')}>清全部缓存</button>
    </div>
  </section>

  <section class="proxy-panel">
    <label for="proxyInput">Steam 代理</label>
    <input id="proxyInput" bind:value={proxyUrl} spellcheck="false" placeholder="http://127.0.0.1:7890" />
    <label for="delayInput">间隔 ms</label>
    <input id="delayInput" bind:value={requestDelayMs} type="number" min="0" max="10000" step="100" />
    <button type="button" on:click={saveSettings}>保存设置</button>
    <button type="button" on:click={testProxy} disabled={proxyTesting}>{proxyTesting ? '测试中' : '测试'}</button>
    <span>{proxyStatus}</span>
  </section>

  <section class="workspace">
    <aside class="input-pane">
      <label for="nameInput">游戏名称</label>
      <textarea id="nameInput" bind:value={nameInput} spellcheck="false" placeholder={'Corpse Keeper\nWarhammer 40,000: Rogue Trader(2026.7.1到期）'}></textarea>
      <div class="input-actions">
        <button type="button" on:click={() => setNames(parseTextarea())}>载入文本</button>
        <button type="button" on:click={() => { nameInput = ''; setNames([]) }}>清空</button>
      </div>
    </aside>

    <section class="result-pane">
      <div class="result-tools">
        <button type="button" on:click={openQueuedLinks} disabled={!canOpen}>队列打开链接</button>
        <label for="openScopeSelect">范围</label>
        <select id="openScopeSelect" bind:value={openScope}>
          <option value="all">全部有链接</option>
          <option value="review">仅需复核</option>
          <option value="matched">仅已匹配</option>
        </select>
        <label for="openDelayInput">打开间隔 ms</label>
        <input id="openDelayInput" bind:value={openDelayMs} type="number" min="100" max="10000" step="100" />
        <label class="check-label">
          <input bind:checked={reviewOnly} type="checkbox" />
          只显示需复核
        </label>
      </div>

      <div class="progress-wrap">
        <div class="progress-label">
          <span>{matching ? progressText : total ? `已处理 ${done}/${total}` : '等待开始'}</span>
          <span>需复核 {reviewCount}</span>
        </div>
        <div class="progress-track"><div style:width={progressWidth}></div></div>
      </div>

      <div class="table-wrap" bind:clientHeight={tableViewportHeight} on:scroll={handleTableScroll}>
        <table>
          <thead>
            <tr>
              <th>原始输入</th>
              <th>清洗后名称</th>
              <th>状态</th>
              <th>Steam 游戏名</th>
              <th>链接</th>
              <th>分数</th>
              <th>操作</th>
            </tr>
          </thead>
          <tbody>
            {#if !total || !visibleResults.length}
              <tr class="empty-row"><td colspan="7">{total ? '没有符合筛选条件的记录' : '暂无记录'}</td></tr>
            {:else}
              {#if topSpacerHeight}
                <tr class="spacer-row" style={`height:${topSpacerHeight}px`}><td colspan="7"></td></tr>
              {/if}
              {#each virtualResults as { item, index } (index)}
                <tr>
                  <td><div class="cell-text" title={item.original}>{item.original}</div></td>
                  <td><div class="cell-text" title={item.cleaned}>{item.cleaned}</div></td>
                  <td><span class={`pill ${statusClass(item.status)}`}>{item.status}</span></td>
                  <td><div class="cell-text" title={item.steamTitle}>{item.steamTitle}</div></td>
                  <td class="link-cell">{#if item.url}<a href={item.url} title={item.url} on:click|preventDefault={() => openExternalUrl(item.url)}>{shortSteamUrl(item)}</a>{:else}<span class="cell-text" title={item.message}>{item.message}</span>{/if}</td>
                  <td>{item.score || ''}</td>
                  <td>
                    <div class="row-actions">
                      {#if item.needsReview && item.url}<button type="button" on:click={() => confirmItem(index)}>确认</button>{/if}
                      <button type="button" on:click={() => openReview(index)}>复核</button>
                      {#if item.url}<button type="button" on:click={() => openExternalUrl(item.url)}>打开</button>{/if}
                    </div>
                  </td>
                </tr>
              {/each}
              {#if bottomSpacerHeight}
                <tr class="spacer-row" style={`height:${bottomSpacerHeight}px`}><td colspan="7"></td></tr>
              {/if}
            {/if}
          </tbody>
        </table>
      </div>
    </section>
  </section>
</main>

{#if modalOpen && current}
  <div
    class="modal-backdrop"
    role="button"
    tabindex="0"
    on:click={(event) => event.target === event.currentTarget && closeReview()}
    on:keydown={(event) => event.key === 'Escape' && closeReview()}
  >
    <section class="modal" role="dialog" aria-modal="true">
      <header>
        <div>
          <h2>{current.original}</h2>
          <p>{current.cleaned || current.original} · {current.status}</p>
        </div>
        <button type="button" class="icon-btn" on:click={closeReview}>×</button>
      </header>
      <div class="candidate-list">
        {#if !current.candidates.length}
          <article class="candidate"><div><strong>无候选</strong><span>可以手动粘贴 Steam 游戏本体链接</span></div></article>
        {:else}
          {#each current.candidates as candidate}
            <article class="candidate">
              <div>
                <strong>{candidate.title}</strong>
                <span>App ID {candidate.appId} · 分数 {candidate.score} · {candidate.url}</span>
              </div>
              <button type="button" on:click={() => chooseCandidate(candidate)}>选择</button>
            </article>
          {/each}
        {/if}
      </div>
      <form class="manual-form" on:submit|preventDefault={saveManual}>
        <label for="manualUrl">手动链接</label>
        <div class="manual-row">
          <input id="manualUrl" bind:value={manualUrl} type="url" placeholder="https://store.steampowered.com/app/..." />
          <button type="submit">保存</button>
        </div>
        <p>保存时会自动读取 Steam 页面标题。</p>
      </form>
    </section>
  </div>
{/if}

{#if toast}
  <div class="toast">{toast}</div>
{/if}
