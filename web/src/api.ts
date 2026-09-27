// 后端 API 封装：对齐 axum 路由 (server.rs / config_routes.rs /
// media_server_routes.rs / metadata_routes.rs / job_routes.rs / notification_routes.rs)。
// PATCH 语义见 komf-api-models/src/config.rs：
// 缺省字段=保持原值，显式 null=清空，所以保存时只发送 diff。
export type ServerKind = 'komga' | 'kavita' | 'stump';

async function req<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(path, {
    ...init,
    headers: { 'Content-Type': 'application/json', ...(init?.headers ?? {}) },
  });
  if (res.status === 204) return undefined as unknown as T;
  if (!res.ok) {
    const text = await res.text().catch(() => '');
    throw new Error(`${res.status} ${path}: ${text.slice(0, 300)}`);
  }
  return (await res.json()) as T;
}

export const api = {
  getConfig: () => req<any>('/api/config'),
  patchConfig: (patch: any) =>
    fetch('/api/config', {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(patch),
    }).then(async (res) => {
      if (res.status === 204) return;
      const text = await res.text().catch(() => '');
      throw new Error(`PATCH /api/config ${res.status}: ${text.slice(0, 500)}`);
    }),
  version: () => req<{ name: string; version: string }>('/api/health'),
  connected: (s: ServerKind) =>
    req<{ success: boolean; httpStatusCode?: number | null; errorMessage?: string | null }>(
      `/api/${s}/media-server/connected`,
    ),
  libraries: (s: ServerKind) =>
    req<Array<{ id: string; name: string }>>(`/api/${s}/media-server/libraries`).catch(() => []),
  providers: (s: ServerKind, libraryId?: string) =>
    req<string[]>(
      `/api/${s}/metadata/providers${libraryId ? `?libraryId=${encodeURIComponent(libraryId)}` : ''}`,
    ).catch(() => []),

  // ---- 搜索 / 识别 ----
  search: (s: ServerKind, name: string, libraryId?: string, seriesId?: string) => {
    const q = new URLSearchParams({ name });
    if (libraryId) q.set('libraryId', libraryId);
    if (seriesId) q.set('seriesId', seriesId);
    return req<any[]>(`/api/${s}/metadata/search?${q}`);
  },
  /** POST /metadata/identify：{libraryId?, seriesId, provider, providerSeriesId} → {id: jobId} */
  identify: (s: ServerKind, body: { libraryId?: string; seriesId: string; provider: string; providerSeriesId: string }) =>
    req<{ id: { value?: string } | string }>(`/api/${s}/metadata/identify`, {
      method: 'POST',
      body: JSON.stringify(body),
    }).then((r) => {
      const v: any = r?.id;
      return typeof v === 'string' ? v : v?.value ?? '';
    }),

  // ---- Jobs：列表 / 单查 / 清空 ----
  jobs: (status?: string) =>
    req<{ content: any[]; totalPages: number; currentPage: number }>(
      `/api/jobs${status ? `?status=${status}&pageSize=200&page=1` : '?pageSize=200&page=1'}`,
    ).catch(() => ({ content: [], totalPages: 0, currentPage: 0 })),
  job: (id: string) => req<any>(`/api/jobs/${id}`).catch(() => null),
  deleteJobs: () => fetch('/api/jobs/all', { method: 'DELETE' }).then((r) => r.status === 204),

  // ---- 触发 匹配 / 重置 ----
  matchSeries: (s: ServerKind, libraryId: string, seriesId: string) =>
    req<{ id: string }>(`/api/${s}/metadata/match/library/${encodeURIComponent(libraryId)}/series/${encodeURIComponent(seriesId)}`, {
      method: 'POST',
    }).then((r) => r?.id ?? ''),
  matchLibrary: (s: ServerKind, libraryId: string) =>
    fetch(`/api/${s}/metadata/match/library/${encodeURIComponent(libraryId)}`, { method: 'POST' }).then(async (r) => {
      if (r.status === 200 || r.status === 202) return r.status;
      const text = await r.text().catch(() => '');
      throw new Error(`${r.status}: ${text.slice(0, 300)}`);
    }),
  resetSeries: (s: ServerKind, libraryId: string, seriesId: string, removeComicInfo: boolean) =>
    fetch(
      `/api/${s}/metadata/reset/library/${encodeURIComponent(libraryId)}/series/${encodeURIComponent(seriesId)}?removeComicInfo=${removeComicInfo}`,
      { method: 'POST' },
    ).then(async (r) => {
      if (r.status === 204) return;
      const text = await r.text().catch(() => '');
      throw new Error(`${r.status}: ${text.slice(0, 300)}`);
    }),
  resetLibrary: (s: ServerKind, libraryId: string, removeComicInfo: boolean) =>
    fetch(
      `/api/${s}/metadata/reset/library/${encodeURIComponent(libraryId)}?removeComicInfo=${removeComicInfo}`,
      { method: 'POST' },
    ).then(async (r) => {
      if (r.status === 204) return;
      const text = await r.text().catch(() => '');
      throw new Error(`${r.status}: ${text.slice(0, 300)}`);
    }),

  // ---- 通知模板 / 渲染 / 发送 ----
  getDiscordTemplates: () =>
    req<{ title?: string; titleUrl?: string; description?: string; footer?: string; fields: any[] }>(
      '/api/notifications/discord/templates',
    ),
  updateDiscordTemplates: (t: any) => req('/api/notifications/discord/templates', { method: 'POST', body: JSON.stringify(t) }),
  getAppriseTemplates: () => req<{ title?: string; body?: string }>('/api/notifications/apprise/templates'),
  updateAppriseTemplates: (t: any) => req('/api/notifications/apprise/templates', { method: 'POST', body: JSON.stringify(t) }),
  renderDiscord: (body: any) => req<any>('/api/notifications/discord/render', { method: 'POST', body: JSON.stringify(body) }),
  sendDiscord: (body: any) => req('/api/notifications/discord/send', { method: 'POST', body: JSON.stringify(body) }),
  renderApprise: (body: any) => req<any>('/api/notifications/apprise/render', { method: 'POST', body: JSON.stringify(body) }),
  sendApprise: (body: any) => req('/api/notifications/apprise/send', { method: 'POST', body: JSON.stringify(body) }),
};

/** SSE 事件流解析：GET /api/jobs/:id/events → 逐事件回调。
 * 服务端事件：ProviderSeriesEvent/ProviderBookEvent/ProviderErrorEvent/
 * ProviderCompletedEvent/PostProcessingStartEvent/CompletionEvent(不发送，直接关流)/
 * EventStreamNotFoundEvent(空 data，job 不存在)。每 15s 有 keep-alive 注释行。 */
export async function streamJobEvents(
  id: string,
  onEvent: (ev: { event: string; data: string }) => void,
  signal?: AbortSignal,
): Promise<void> {
  const res = await fetch(`/api/jobs/${encodeURIComponent(id)}/events`, {
    headers: { Accept: 'text/event-stream' },
    signal,
  });
  if (!res.ok) {
    const text = await res.text().catch(() => '');
    throw new Error(`SSE ${res.status}: ${text.slice(0, 200)}`);
  }
  if (!res.body) throw new Error('SSE: 无响应体');
  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let buf = '';
  let evName = '';
  let evData: string[] = [];
  const dispatch = () => {
    if (evName || evData.length) {
      onEvent({ event: evName || 'message', data: evData.join('\n') });
      evName = '';
      evData = [];
    }
  };
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    buf += decoder.decode(value, { stream: true });
    let idx: number;
    while ((idx = buf.indexOf('\n')) >= 0) {
      const line = buf.slice(0, idx).replace(/\r$/, '');
      buf = buf.slice(idx + 1);
      if (line === '') dispatch();
      else if (line.startsWith(':')) continue; // keep-alive 注释
      else if (line.startsWith('event:')) evName = line.slice(6).trim();
      else if (line.startsWith('data:')) evData.push(line.slice(5).trimStart());
      // 其余字段（id/retry）忽略
    }
  }
  dispatch();
}

/** 触发离线 DB 下载（MangaBaka/BookWalker）：POST /api/update-*-db，响应为 NDJSON 进度流。
 *  每行一个事件：{"type":"ProgressEvent","total","completed","info"} / {"type":"FinishedEvent"} / {"type":"ErrorEvent","message"} */
export async function updateDb(
  kind: 'manga-baka' | 'book-walker',
  onEvent: (ev: { type: string; total?: number; completed?: number; info?: string | null; message?: string }) => void,
  signal?: AbortSignal,
): Promise<void> {
  const res = await fetch(`/api/update-${kind}-db`, { method: 'POST', signal });
  if (!res.ok) {
    const text = await res.text().catch(() => '');
    throw new Error(`下载 DB ${res.status}: ${text.slice(0, 200)}`);
  }
  if (!res.body) throw new Error('下载 DB: 无响应体');
  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let buf = '';
  const emit = (line: string) => {
    const t = line.trim();
    if (!t) return;
    try { onEvent(JSON.parse(t)); } catch { /* 忽略非 JSON 行 */ }
  };
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    buf += decoder.decode(value, { stream: true });
    let idx: number;
    while ((idx = buf.indexOf('\n')) >= 0) {
      emit(buf.slice(0, idx).replace(/\r$/, ''));
      buf = buf.slice(idx + 1);
    }
  }
  if (buf.trim()) emit(buf);
}