// 轻量 i18n：中/英双语，module 级状态 + 广播，组件共享同一语言。
// 语言来源优先级：localStorage 'komf-lang' > navigator.language（zh 前缀 → 中文，否则英文）。
import { useEffect, useState } from 'react';

export type Lang = 'zh' | 'en';

function initLang(): Lang {
  try {
    const saved = localStorage.getItem('komf-lang');
    if (saved === 'zh' || saved === 'en') return saved;
  } catch { /* ignore */ }
  return typeof navigator !== 'undefined' && /^zh/i.test(navigator.language) ? 'zh' : 'en';
}

let current: Lang = initLang();
const listeners = new Set<() => void>();

const dict: Record<string, { zh: string; en: string }> = {
  // ---- 顶栏 / 导航 ----
  'app.title': { zh: 'komf-rs 配置', en: 'komf-rs Config' },
  'sync.clean': { zh: '已同步', en: 'Synced' },
  'sync.dirty': { zh: '有未保存改动', en: 'Unsaved changes' },
  'theme.title.system': { zh: '主题:跟随系统', en: 'Theme: system' },
  'theme.title.light': { zh: '主题:浅色', en: 'Theme: light' },
  'theme.title.dark': { zh: '主题:深色', en: 'Theme: dark' },
  'lang.title.zh': { zh: '切换为 English', en: 'Switch to 中文' },
  'lang.title.en': { zh: '当前 English，点击切回中文', en: 'Currently English, click for 中文' },
  'lang.cur.zh': { zh: '中文', en: '中文' },
  'lang.cur.en': { zh: 'English', en: 'English' },
  'reload': { zh: '重载', en: 'Reload' },
  'saving': { zh: '保存中…', en: 'Saving…' },
  'savePatch': { zh: '保存 PATCH', en: 'Save PATCH' },
  'tab.overview': { zh: '概览', en: 'Overview' },
  'tab.servers': { zh: '媒体服务器', en: 'Media servers' },
  'tab.providers': { zh: 'Providers', en: 'Providers' },
  'tab.metadata': { zh: '元数据', en: 'Metadata' },
  'tab.library': { zh: '按库覆盖', en: 'Per-library' },
  'tab.notifications': { zh: '通知', en: 'Notifications' },
  'tab.jobs': { zh: 'Jobs', en: 'Jobs' },
  'tab.patch': { zh: 'PATCH预览', en: 'PATCH preview' },

  // ---- 通用控件 ----
  'on': { zh: '开', en: 'On' },
  'off': { zh: '关', en: 'Off' },
  'keep': { zh: '(保持原值)', en: '(keep)' },
  'tri.clear1': { zh: '清空(null)，后端回默认/删除', en: 'Clear (null): back to default / delete' },
  'tri.clear2': { zh: '清空(null)，回后端默认', en: 'Clear (null): back to default' },
  'remove': { zh: '移除', en: 'Remove' },
  'add': { zh: '+ 添加', en: '+ Add' },
  'addField': { zh: '+ 添加 field', en: '+ Add field' },
  'none': { zh: '未选择', en: 'None' },
  'collapse': { zh: '收起', en: 'Collapse' },
  'expand': { zh: '展开', en: 'Expand' },

  // ---- Provider 网格 ----
  'priority': { zh: '优先级', en: 'Priority' },
  'ph.tagWhitelistFile': { zh: '可选，标签白名单文件路径', en: 'optional, tag whitelist file path' },
  'ph.exJaZh': { zh: '如 ja, zh', en: 'e.g. ja, zh' },
  'ph.tagTranslationUrl': { zh: '留空=官方 release', en: 'blank = official release' },
  'badge.seriesMetaDefault': { zh: '默认全开(score/useOriginalPublisher 除外)', en: 'all on by default (except score / useOriginalPublisher)' },
  'sensitive': { zh: '敏感', en: 'sensitive' },
  'f.coverLanguages': { zh: 'coverLanguages(逗号)', en: 'coverLanguages (comma-separated)' },
  'f.tagWhitelist': { zh: 'tagWhitelist(逗号)', en: 'tagWhitelist (comma-separated)' },
  'f.preferredLanguages': { zh: 'preferredLanguages(逗号)', en: 'preferredLanguages (comma-separated)' },
  'f.translatorKeywords': { zh: 'translatorKeywords(逗号)', en: 'translatorKeywords (comma-separated)' },

  // ---- 事件流 ----
  'ev.connecting': { zh: 'SSE 连接中', en: 'SSE connected' },
  'ev.done': { zh: '流已结束', en: 'Stream ended' },
  'ev.error': { zh: '流错误', en: 'Stream error' },
  'ev.waiting': { zh: '等待事件…(服务端 15s keep-alive；已完成的任务可能直接关流)', en: 'Waiting for events… (server keep-alive 15s; finished jobs may close the stream)' },

  // ---- toast / 状态消息 ----
  'err.loadFailed': { zh: '加载失败: {msg}', en: 'Load failed: {msg}' },
  'err.scopeJson': { zh: '按库覆盖 JSON 解析失败: {msg}', en: 'Per-library JSON parse failed: {msg}' },
  'err.jobs': { zh: '任务列表加载失败: {msg}', en: 'Job list load failed: {msg}' },
  'err.clear': { zh: '清空失败: {msg}', en: 'Clear failed: {msg}' },
  'err.ctxJson': { zh: '测试上下文 JSON 解析失败: {msg}', en: 'Test context JSON parse failed: {msg}' },
  'save.noChange': { zh: '没有改动，无需保存。', en: 'No changes to save.' },
  'save.done': { zh: '已保存并热重载。', en: 'Saved and hot-reloaded.' },
  'db.failed': { zh: '下载失败', en: 'Download failed' },
  'db.downloading': { zh: '下载中…', en: 'Downloading…' },
  'db.downloadDb': { zh: '下载 DB', en: 'Download DB' },
  'db.undownloaded': { zh: '未下载', en: 'not downloaded' },
  'db.err': { zh: 'DB 下载失败：{msg}', en: 'DB download failed: {msg}' },
  'db.desc': { zh: 'DB 为离线匹配用的全量数据库，点「下载 DB」触发更新（POST /api/update-*-db，jsonl 进度流）；完成后状态徽标自动刷新。', en: 'DBs are full offline databases used for matching. Click "Download DB" to trigger an update (POST /api/update-*-db, NDJSON progress stream); status badges refresh automatically when done.' },

  // ---- 概览 ----
  'ov.status': { zh: '服务状态', en: 'Service status' },
  'ov.connDesc': { zh: '连通性调 GET /api/{servers}/media-server/connected；库列表调 libraries。', en: 'Connectivity via GET /api/{servers}/media-server/connected; libraries via libraries.' },
  'unset': { zh: '(未配)', en: '(unset)' },
  'ov.connected': { zh: '连通 · 库 {n} 个', en: 'Connected · {n} librar{ies}' },
  'ov.disconnected': { zh: '未连通', en: 'Disconnected' },
  'ov.enabledProviders': { zh: '启用 Provider {a}/{b}', en: 'Providers enabled {a}/{b}' },
  'ov.mangaBakaDb': { zh: 'MangaBaka DB: {v}', en: 'MangaBaka DB: {v}' },
  'ov.bookWalkerDb': { zh: 'BookWalker DB: {v}', en: 'BookWalker DB: {v}' },
  'ov.search': { zh: '搜索试跑', en: 'Search trial' },
  'ov.searchDesc': { zh: '直接调后端搜索，验证当前启用 Provider 与优先级。填系列 ID 后每条结果可一键设元数据(identify)。', en: 'Search the backend directly to verify enabled providers and priority. Fill in a series ID to identify any result with one click.' },
  'allLibs': { zh: '全部库', en: 'All libraries' },
  'ph.seriesName': { zh: '系列名，如 葬送的芙莉莲', en: 'Series name, e.g. Sousou no Frieren' },
  'ph.seriesId': { zh: '系列 ID(可选，设元数据需要)', en: 'Series ID (optional, needed to identify)' },
  'searching': { zh: '搜索中…', en: 'Searching…' },
  'search': { zh: '搜索', en: 'Search' },
  'hits': { zh: '共 {n} 条结果', en: '{n} results' },
  'noResults': { zh: '无结果', en: 'No results' },
  'source': { zh: '来源 ↗', en: 'source ↗' },
  'identify.needId': { zh: '需先填系列 ID', en: 'Series ID required' },
  'identify.title': { zh: 'identify 写入该系列元数据', en: 'Identify: write metadata to this series' },
  'identify': { zh: '设元数据', en: 'Identify' },

  // ---- 媒体服务器 ----
  'ms.desc': { zh: '密码/API Key 后端 GET 不返回；留空=保持原值，填写=覆盖。对应 {s}.baseUri / komgaUser / apiKey。', en: 'Passwords / API keys are never returned by GET; blank = keep, fill = override. Maps to {s}.baseUri / komgaUser / apiKey.' },
  'f.komgaPassword': { zh: 'komgaPassword(留空=保持)', en: 'komgaPassword (blank = keep)' },
  'f.komgaApiKey': { zh: 'komgaApiKey(留空=保持)', en: 'komgaApiKey (blank = keep)' },
  'f.apiKey': { zh: 'apiKey(留空=保持)', en: 'apiKey (blank = keep)' },
  'f.password': { zh: 'password(留空=保持)', en: 'password (blank = keep)' },
  'notConfigured': { zh: '未配置', en: 'not set' },
  'f.metadataLibraryFilter': { zh: 'metadataLibraryFilter', en: 'metadataLibraryFilter' },
  'f.metadataSeriesExcludeFilter': { zh: 'metadataSeriesExcludeFilter', en: 'metadataSeriesExcludeFilter' },
  'ph.emptyAll': { zh: '空=全部', en: 'empty = all' },

  // ---- Providers ----
  'providers.title': { zh: '默认 Providers', en: 'Default Providers' },
  'providers.desc': { zh: '开关+优先级。数字越小越先匹配。特有字段点展开。对应 metadataProviders.defaultProviders.*。', en: 'Toggle + priority. Lower number matches first. Provider-specific fields expand on click. Maps to metadataProviders.defaultProviders.*.' },
  'f.nameMatchingMode': { zh: '全局 nameMatchingMode', en: 'Global nameMatchingMode' },
  'f.malClientId': { zh: 'malClientId(留空=保持)', en: 'malClientId (blank = keep)' },
  'f.comicVineClientId': { zh: 'comicVineClientId(留空=保持)', en: 'comicVineClientId (blank = keep)' },
  'f.bangumiToken': { zh: 'bangumiToken(留空=保持)', en: 'bangumiToken (blank = keep)' },
  'ph.nsfw': { zh: '可选，显示NSFW', en: 'optional, shows NSFW' },
  'f.comicVineSearchLimit': { zh: 'comicVineSearchLimit', en: 'comicVineSearchLimit' },
  'ph.ex20': { zh: '如 20', en: 'e.g. 20' },
  'f.comicVineIssueName': { zh: 'comicVineIssueName', en: 'comicVineIssueName' },
  'f.comicVineIdFormat': { zh: 'comicVineIdFormat', en: 'comicVineIdFormat' },
  'blankClears': { zh: '留空=清空', en: 'blank = clear' },
  'ov.mangaBakaChecksum': { zh: 'MangaBaka DB: {v} / checksum {c}', en: 'MangaBaka DB: {v} / checksum {c}' },

  // ---- OAuth ----
  'oauth.title': { zh: 'OAuth 登录（共享 client）', en: 'OAuth sign-in (shared client)' },
  'oauth.desc': { zh: '登录后请求携带 Bearer token：MAL 无需 malClientId、Bangumi token 自动续期、AniList 解锁更高限额。', en: 'Requests carry a Bearer token after sign-in: MAL works without malClientId, Bangumi tokens auto-renew, AniList unlocks higher limits.' },
  'oauth.login': { zh: '登录', en: 'Sign in' },
  'oauth.logout': { zh: '退出', en: 'Sign out' },
  'oauth.loggedInAs': { zh: '{u}', en: '{u}' },
  'oauth.notLoggedIn': { zh: '未登录', en: 'Not signed in' },
  'oauth.success': { zh: 'OAuth 登录成功。', en: 'OAuth sign-in successful.' },
  'oauth.failed': { zh: 'OAuth 登录失败：{msg}', en: 'OAuth sign-in failed: {msg}' },
  'oauth.checking': { zh: '检查中…', en: 'Checking…' },

  // ---- Tracker（阅读状态同步） ----
  'tab.tracker': { zh: 'Tracker', en: 'Tracker' },
  'tr.title': { zh: 'Tracker 阅读状态同步', en: 'Tracker sync' },
  'tr.desc': { zh: '以 OAuth 登录态调用平台用户列表 API：搜索条目 → 选中 → 编辑状态/进度/评分并推送。进度值由本页手动提供（komf 无阅读器事件）。', en: 'Uses the OAuth session to call each platform user-list API: search → select → edit status/progress/score and push. Progress is provided manually (komf has no reader events).' },
  'tr.loggedIn': { zh: '已登录', en: 'Signed in' },
  'tr.notLoggedIn': { zh: '未登录（先点登录）', en: 'Not signed in (sign in first)' },
  'tr.loginLost': { zh: '登录状态已失效，请重新登录后继续同步。', en: 'Tracker sign-in expired — sign in again to keep syncing.' },
  'tr.relogin': { zh: '重新登录', en: 'Sign in again' },
  'tr.phQuery': { zh: '标题或条目链接（如 anilist.co/manga/87395）', en: 'Title or item link (e.g. anilist.co/manga/87395)' },
  'tr.nsfw': { zh: '包含 NSFW', en: 'Include NSFW' },
  'tr.search': { zh: '搜索', en: 'Search' },
  'tr.results': { zh: '{n} 条结果', en: '{n} results' },
  'tr.tracked': { zh: '已在列表', en: 'tracked' },
  'tr.pick': { zh: '选择', en: 'Select' },
  'tr.selected': { zh: '选中条目: {id}', en: 'Selected: {id}' },
  'tr.state': { zh: '当前状态', en: 'Current' },
  'tr.noEntry': { zh: '尚未加入列表，推送后自动创建。', en: 'Not in list yet; pushing will create it.' },
  'tr.stateLoading': { zh: '正在读取平台状态…', en: 'Loading platform state…' },
  'tr.scoreHint': { zh: '评分口径：AniList 0-100；MAL/Bangumi 0-10。留空字段=不推送该字段。', en: 'Score scale: AniList 0-100; MAL/Bangumi 0-10. Blank fields are not pushed.' },
  'tr.score': { zh: '评分', en: 'Score' },
  'tr.status': { zh: '状态', en: 'Status' },
  'tr.chapter': { zh: '已读章节', en: 'Chapters read' },
  'tr.volume': { zh: '已读卷', en: 'Volumes read' },
  'tr.startDate': { zh: '开始日期', en: 'Start date' },
  'tr.finishDate': { zh: '完成日期', en: 'Finish date' },
  'tr.push': { zh: '推送更新', en: 'Push update' },
  'tr.pushed': { zh: '已推送，状态已刷新。', en: 'Pushed; state refreshed.' },
  'tr.links': { zh: '已关联', en: 'Linked' },
  'tr.linksDesc': { zh: '通过 komf API 推送过的条目（本地台账，点击可查看/编辑状态）', en: 'Items pushed through the komf API (local record; click to view/edit state)' },
  'tr.linksEmpty': { zh: '暂无已关联条目', en: 'No linked items yet' },

  // ---- 元数据 ----
  'md.defaultTitle': { zh: '{s} · 默认更新策略', en: '{s} · Default update strategy' },
  'md.desc': { zh: '对应 {s}.metadataUpdate.default。', en: 'Maps to {s}.metadataUpdate.default.' },
  'f.updateModes': { zh: 'updateModes', en: 'updateModes' },
  'md.advanced': { zh: '高级字段（Rust 扩展 & 长尾）', en: 'Advanced fields (Rust extensions & long tail)' },
  'f.mylarOutputDir': { zh: 'mylarOutputDir(留空=清空)', en: 'mylarOutputDir (blank = clear)' },
  'ph.mylarOutputDir': { zh: 'mylar series.json 导出目录', en: 'mylar series.json export dir' },
  'f.failedMatchCollectionName': { zh: 'failedMatchCollectionName(留空=清空)', en: 'failedMatchCollectionName (blank = clear)' },
  'ph.failedMatchCollection': { zh: 'Auto-Identify 失败系列收藏夹', en: 'collection for Auto-Identify failed series' },
  'f.altSeriesTitleLangs': { zh: 'alternativeSeriesTitleLanguages(逗号)', en: 'alternativeSeriesTitleLanguages (comma-separated)' },
  'f.originalPublisherTagName': { zh: 'originalPublisherTagName(留空=清空)', en: 'originalPublisherTagName (blank = clear)' },
  'f.publisherTagNames': { zh: 'postProcessing.publisherTagNames(tagName 必填)', en: 'postProcessing.publisherTagNames (tagName required)' },
  'hint.langJaEn': { zh: '语言如 ja/en', en: 'language e.g. ja/en' },
  'md.chineseConversion': { zh: 'chineseConversion（简繁转换扩展）', en: 'chineseConversion (Simplified/Traditional conversion)' },
  'md.ccSearch': { zh: 'chineseConversion.search(搜索关键词)', en: 'chineseConversion.search (search keyword)' },
  'md.ccMatching': { zh: 'chineseConversion.matching(自动匹配)', en: 'chineseConversion.matching (auto match)' },
  'md.ccUpdateFields': { zh: 'chineseConversion.update.fields(应用字段)', en: 'chineseConversion.update.fields (applied fields)' },
  'md.searchTitleExtraction': { zh: 'searchTitleExtraction（搜索标题提取）', en: 'searchTitleExtraction (search title extraction)' },
  'f.bracketRegex': { zh: 'bracketRegex(留空=清空)', en: 'bracketRegex (blank = clear)' },
  'ph.bracketRegex': { zh: '如 \\[[^\\]]+\\]', en: 'e.g. \\[[^\\]]+\\]' },
  'f.authorSeparator': { zh: 'authorSeparator(留空=清空)', en: 'authorSeparator (blank = clear)' },
  'f.titleSplitters': { zh: 'titleSplitters(逗号)', en: 'titleSplitters (comma-separated)' },
  'f.cleanupRegex': { zh: 'cleanupRegex(逗号)', en: 'cleanupRegex (comma-separated)' },
  'f.charMappings': { zh: 'charMappings(单字符映射，如 ／→/)', en: 'charMappings (single-char mapping, e.g. ／→/)' },
  'ph.srcChar': { zh: '源字符', en: 'source char' },
  'ph.dstChar': { zh: '替换为', en: 'replace with' },

  // ---- 按库覆盖 ----
  'lib.title': { zh: '按库覆盖', en: 'Per-library overrides' },
  'lib.desc': { zh: '库 id 从连通性拉取。两类覆盖独立：元数据处理(metadataUpdate.library.{id}) 与 Provider 开关/优先级(metadataProviders.libraryProviders.{id})。保存空对象/删除覆盖=发 null 删除该库配置。', en: 'Library ids are fetched from connectivity. Two independent override types: metadata processing (metadataUpdate.library.{id}) and Provider toggles/priority (metadataProviders.libraryProviders.{id}). Saving an empty object / deleting the override sends null to remove that library config.' },
  'selectLib': { zh: '选择库…', en: 'Select library…' },
  'lib.copySkeleton': { zh: '从默认复制骨架(元数据)', en: 'Copy skeleton from default (metadata)' },
  'lib.delMeta': { zh: '删除元数据覆盖', en: 'Delete metadata override' },
  'lib.copyFail': { zh: '默认 Providers 为空，无法复制', en: 'Default Providers is empty, cannot copy' },
  'lib.copyProv': { zh: '从默认复制(Provider)', en: 'Copy from default (Providers)' },
  'lib.delProv': { zh: '删除 Provider 覆盖', en: 'Delete Provider override' },
  'lib.metaBadge': { zh: 'metadataUpdate.library 覆盖: {ids}', en: 'metadataUpdate.library overrides: {ids}' },
  'lib.provBadge': { zh: 'libraryProviders 覆盖: {ids}', en: 'libraryProviders overrides: {ids}' },
  'noneList': { zh: '无', en: 'none' },
  'lib.metaTitle': { zh: '元数据处理覆盖 metadataUpdate.library[{lib}]', en: 'Metadata override metadataUpdate.library[{lib}]' },
  'lib.metaDesc': { zh: 'JSON 编辑（骨架复制可省手写）。字段同「元数据」页默认策略。', en: 'JSON editing (copy skeleton to avoid handwriting). Fields match the default strategy on the Metadata page.' },
  'lib.provDesc': { zh: 'Provider 开关/优先级覆盖 libraryProviders[{lib}] —— 未勾选/留空的项继承默认 Providers 配置。', en: 'Provider toggle/priority overrides libraryProviders[{lib}] — unchecked/blank items inherit the default Providers config.' },
  'lib.unselected': { zh: '未选', en: 'unselected' },

  // ---- 通知 ----
  'notif.urls': { zh: 'Discord / Apprise 地址', en: 'Discord / Apprise URLs' },
  'notif.urlsDesc': { zh: '后端 GET 会脱敏显示；此处填写完整 URL 保存，留空=保持。后端按索引合并。', en: 'GET masks values; fill full URLs to save, blank = keep. Merged by index on the backend.' },
  'f.webhooks': { zh: 'discord.webhooks(每行一个，留空=保持)', en: 'discord.webhooks (one per line, blank = keep)' },
  'f.urls': { zh: 'apprise.urls(每行一个，留空=保持)', en: 'apprise.urls (one per line, blank = keep)' },
  'notif.discordTpl': { zh: 'Discord 通知模板', en: 'Discord notification templates' },
  'notif.tplDesc': { zh: 'Velocity 模板，变量如 $library.name / $series.name / $series.metadata.summary / $books.size() / $mediaServer。保存即热生效；「渲染预览/发送测试」会带上当前未保存的改动（仅本次生效）。首次预览为空属正常（模板文件尚未落盘），先点一次「保存模板」即有输出。', en: 'Velocity templates; variables like $library.name / $series.name / $series.metadata.summary / $books.size() / $mediaServer. Save applies immediately; "Render preview"/"Send test" include unsaved edits (this session only). An empty first preview is normal (template file not written yet) — click "Save template" once and output appears.' },
  'discard': { zh: '放弃改动', en: 'Discard changes' },
  'notif.saveTpl': { zh: '保存模板', en: 'Save template' },
  'notif.fieldsTitle': { zh: 'fields（name / value / inline）', en: 'fields (name / value / inline)' },
  'notif.renderPreview': { zh: '渲染预览', en: 'Render preview' },
  'notif.sendTest': { zh: '发送测试', en: 'Send test' },
  'notif.appriseTpl': { zh: 'Apprise 通知模板', en: 'Apprise notification templates' },
  'notif.appriseTplDesc': { zh: '同上 Velocity 变量；Apprise 仅 title / body 两段。', en: 'Same Velocity variables; Apprise has only title / body.' },
  'notif.ctx': { zh: '测试上下文', en: 'Test context' },
  'notif.ctxDesc': { zh: '渲染预览与发送测试都使用此上下文；字段对齐 KomfNotificationContext（camelCase）。', en: 'Used by both render preview and send test; fields match KomfNotificationContext (camelCase).' },
  'notif.resetCtx': { zh: '重置为示例', en: 'Reset to sample' },
  'notif.renderTitle': { zh: '渲染预览({ch})', en: 'Render preview ({ch})' },
  'notif.saved': { zh: '{ch} 模板已保存并热生效。', en: '{ch} templates saved and hot-applied.' },
  'notif.sent': { zh: '已发送测试通知({ch})。', en: 'Test notification sent ({ch}).' },
  'noTitle': { zh: '(无标题)', en: '(no title)' },

  // ---- Jobs ----
  'jobs.trigger': { zh: '触发 匹配 / 重置', en: 'Trigger match / reset' },
  'jobs.triggerDesc': { zh: '库级为后台任务（不返回 job id，稍等后刷新列表查看）；系列级返回 job id 并自动订阅事件流。接口：POST /metadata/{match|reset}/library/…。', en: 'Library-level runs as background tasks (no job id; refresh the list later); series-level returns a job id and subscribes to the event stream. API: POST /metadata/{match|reset}/library/….' },
  'ph.seriesIdReq': { zh: '系列 ID(系列级操作必填)', en: 'Series ID (required for series-level)' },
  'jobs.removeComicInfo': { zh: '重置时移除 ComicInfo', en: 'Remove ComicInfo on reset' },
  'jobs.matchSeries': { zh: '匹配系列', en: 'Match series' },
  'jobs.matchLib': { zh: '匹配全库', en: 'Match library' },
  'jobs.resetSeries': { zh: '重置系列', en: 'Reset series' },
  'jobs.resetLib': { zh: '重置全库', en: 'Reset library' },
  'jobs.needLib': { zh: '请先选择库', en: 'Select a library first' },
  'jobs.needSeriesId': { zh: '系列级操作需要填写系列 ID', en: 'Series-level actions need a series ID' },
  'jobs.seriesTriggered': { zh: '已触发系列匹配 → job {id}，下方事件流实时更新', en: 'Series match triggered → job {id}, live in the event stream below' },
  'jobs.libTriggered': { zh: '已触发全库匹配(后台任务，无 job id)，3 秒后自动刷新任务列表', en: 'Library match triggered (background task, no job id); list refreshes in 3s' },
  'jobs.seriesReset': { zh: '系列元数据已重置', en: 'Series metadata reset' },
  'jobs.libReset': { zh: '全库元数据已重置', en: 'Library metadata reset' },
  'jobs.clearConfirm': { zh: '确认清空全部任务记录？此操作不可恢复。', en: 'Clear all job records? This cannot be undone.' },
  'jobs.cleared': { zh: '任务记录已清空。', en: 'Job records cleared.' },
  'jobs.identify': { zh: '已触发 identify → job {id}，Jobs 页事件流实时更新', en: 'Identify triggered → job {id}, live in the Jobs event stream' },
  'jobs.list': { zh: '任务列表', en: 'Job list' },
  'allStatus': { zh: '全部状态', en: 'All statuses' },
  'refresh': { zh: '刷新', en: 'Refresh' },
  'clearAll': { zh: '清空全部', en: 'Clear all' },
  'count': { zh: '{n} 条', en: '{n} items' },
  'jobs.empty': { zh: '暂无任务记录（库级匹配为后台任务，稍后刷新）。', en: 'No job records yet (library-level matches are background tasks; refresh later).' },
  'th.status': { zh: '状态', en: 'Status' },
  'th.series': { zh: '系列', en: 'Series' },
  'th.job': { zh: 'Job', en: 'Job' },
  'th.message': { zh: '消息', en: 'Message' },
  'th.start': { zh: '开始', en: 'Start' },
  'th.end': { zh: '结束', en: 'End' },
  'jobs.events': { zh: '事件流', en: 'Event stream' },
  'close': { zh: '关闭', en: 'Close' },
  'jobs.eventsDesc': { zh: '触发系列操作、identify 或点击任务列表行后在此实时显示 SSE 进度。', en: 'Live SSE progress appears here after triggering a series action, identify, or clicking a job row.' },

  // ---- PATCH 预览 ----
  'patch.title': { zh: '将要发送的 PATCH', en: 'PATCH to be sent' },
  'patch.desc': { zh: '增量语义：缺省=保持，null=清空。凭据空=不发送。', en: 'Incremental semantics: omitted = keep, null = clear. Empty credentials are not sent.' },
  'noChange': { zh: '无改动', en: 'No changes' },
  'yaml': { zh: 'YAML 对照', en: 'YAML view' },
  'copyJson': { zh: '复制 JSON', en: 'Copy JSON' },
  'save': { zh: '保存', en: 'Save' },
  'loading': { zh: '加载中…(后端默认 :8085，dev 会代理 /api)', en: 'Loading… (backend :8085 by default; dev proxies /api)' },
};

export function t(key: string, params?: Record<string, string | number>): string {
  const entry = dict[key] ?? { zh: key, en: key };
  let out = entry[current];
  if (params) {
    for (const [k, v] of Object.entries(params)) {
      out = out.split(`{${k}}`).join(String(v));
    }
  }
  return out;
}

export function useLang() {
  const [, force] = useState(0);
  useEffect(() => {
    const update = () => force((n) => n + 1);
    listeners.add(update);
    return () => { listeners.delete(update); };
  }, []);
  const setLang = (l: Lang) => {
    if (l === current) return;
    current = l;
    try { localStorage.setItem('komf-lang', l); } catch { /* ignore */ }
    listeners.forEach((f) => f());
  };
  return { lang: current, setLang, t };
}
