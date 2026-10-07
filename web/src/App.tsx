import React, { Fragment, useEffect, useMemo, useRef, useState } from 'react';
import { api, DbKind, ServerKind, streamJobEvents, updateDb } from './api';
import { diff, isEmptyPatch } from './diff';
import { useLang } from './i18n';
import * as YAML from 'yaml';

const SERVERS: ServerKind[] = ['komga', 'kavita', 'stump'];
const TABS = ['overview', 'servers', 'providers', 'metadata', 'library', 'tracker', 'notifications', 'jobs', 'patch'] as const;
const SERVER_LABEL: Record<ServerKind, string> = { komga: 'Komga', kavita: 'Kavita', stump: 'Stump' };

// 配置 DTO 中的 Provider key → 展示名 / 提示
const PROVIDER_KEYS = [
  'mangaBaka', 'bookWalker', 'mangaDex', 'mangaUpdates', 'aniList', 'mal', 'comicVine',
  'yenPress', 'viz', 'bangumi', 'webtoons', 'eHentai',
];
const PROVIDER_LABEL: Record<string, string> = {
  mangaBaka: 'MangaBaka', bookWalker: 'BookWalker', mangaDex: 'MangaDex', mangaUpdates: 'MangaUpdates',
  aniList: 'AniList', mal: 'MyAnimeList', comicVine: 'ComicVine',
  yenPress: 'YenPress', viz: 'Viz', bangumi: 'Bangumi',
  webtoons: 'Webtoons', eHentai: 'eHentai',
};
const PROVIDER_HINT: Record<string, string> = {
  mangaUpdates: 'hint.mangaUpdates', mangaDex: '', aniList: '',
  mal: 'hint.mal', bangumi: 'hint.bangumi', eHentai: 'hint.eHentai',
  comicVine: 'hint.comicVine', bookWalker: 'hint.bookWalker', mangaBaka: 'hint.mangaBaka', yenPress: '', viz: '',
  webtoons: '',
};

const SERIES_FIELDS = [
  'status', 'title', 'alternativeTitles', 'titleSort', 'summary', 'publisher',
  'readingDirection', 'ageRating', 'language', 'genres', 'tags', 'totalBookCount',
  'authors', 'releaseDate', 'thumbnail', 'books', 'links', 'score', 'useOriginalPublisher',
];
const BOOK_FIELDS = [
  'title', 'summary', 'number', 'numberSort', 'releaseDate',
  'authors', 'tags', 'isbn', 'links', 'thumbnail',
];
const NAME_MODES = ['EXACT', 'CLOSEST_MATCH']; // DTO 只支持这两种（CONTAINS/DISTANCE 会被后端 422 拒掉）
const AUTHOR_ROLES = ['WRITER', 'PENCILLER', 'INKER', 'COLORIST', 'LETTERER', 'COVER', 'EDITOR', 'TRANSLATOR'];
const CHINESE_FIELDS = ['title', 'genres', 'tags', 'summary'];

// 通知渲染/发送的测试上下文（对齐 KomfNotificationContext camelCase DTO）
const SAMPLE_CTX = {
  library: { id: 'lib-id', name: '示例文库' },
  series: {
    id: 'series-id',
    name: '葬送的芙莉莲',
    bookCount: 12,
    metadata: {
      status: 'ONGOING',
      title: '葬送的芙莉莲',
      titleSort: 'Frieren',
      alternativeTitles: [
        { label: 'Romaji', title: 'Sousou no Frieren' },
        { label: 'Native', title: '葬送のフリーレン' },
      ],
      summary: '勇者一行讨伐魔王之后，精灵魔法使芙莉莲踏上理解人类之旅的故事。',
      readingDirection: 'RIGHT_TO_LEFT',
      publisher: '小学馆',
      alternativePublishers: [],
      ageRating: 13,
      language: 'ja',
      genres: ['奇幻', '冒险'],
      tags: ['魔法'],
      totalBookCount: 12,
      authors: [
        { name: '山田鐘人', role: 'WRITER' },
        { name: 'アベツカサ', role: 'ARTIST' },
      ],
      releaseYear: 2020,
      links: [{ label: 'MangaDex', url: 'https://mangadex.org/title/example' }],
    },
  },
  books: [
    {
      id: 'book-1',
      name: 'Vol.1',
      number: 1,
      metadata: {
        title: '葬送的芙莉莲 Vol.1',
        summary: '第一册。',
        number: '1',
        numberSort: '1',
        releaseDate: '2020-08-18',
        authors: [{ name: '山田鐘人', role: 'WRITER' }],
        tags: ['魔法'],
        isbn: null,
        links: [],
      },
    },
  ],
  mediaServer: 'komga',
};

// provider 封面 <img>：直连 CDN + no-referrer（MangaDex 等封面 CDN 按 Referer
// 白名单拦截，无 Referer 才放行真实封面）；旧浏览器忽略 referrerpolicy 导致
// 加载失败时，回退走 komf /api/cover/redirect 中转（302 + Referrer-Policy）。
function CoverImg(props: { url: string; className?: string }) {
  return (
    <img
      src={props.url}
      referrerPolicy="no-referrer"
      className={props.className}
      alt=""
      onError={(e) => {
        const img = e.target as HTMLImageElement;
        if (!img.dataset.fallback) {
          img.dataset.fallback = '1';
          img.src = `/api/cover/redirect?url=${encodeURIComponent(props.url)}`;
        } else {
          img.style.display = 'none';
        }
      }}
    />
  );
}

function clone<T>(v: T): T {
  return v === undefined ? v : JSON.parse(JSON.stringify(v));
}
function getPath(obj: any, path: (string | number)[]): any {
  return path.reduce((o, k) => (o == null ? o : o[k as any]), obj);
}
function setPath(obj: any, path: (string | number)[], value: any) {
  let o = obj;
  for (let i = 0; i < path.length - 1; i++) {
    const k = path[i] as any;
    if (o[k] == null || typeof o[k] !== 'object') o[k] = typeof path[i + 1] === 'number' ? [] : {};
    o = o[k];
  }
  o[path[path.length - 1] as any] = value;
}

// ---------------- 基础控件 ----------------

function Field(props: { label: string; children: React.ReactNode; hint?: string }) {
  return (
    <div className="field">
      <label>
        {props.label} {props.hint ? <span className="badge">{props.hint}</span> : null}
      </label>
      {props.children}
    </div>
  );
}
function Text(props: { value: any; onChange: (v: string) => void; placeholder?: string; password?: boolean }) {
  return (
    <input
      type={props.password ? 'password' : 'text'}
      value={props.value ?? ''}
      placeholder={props.placeholder}
      onChange={(e) => props.onChange(e.target.value)}
    />
  );
}
function Switch(props: { value: any; onChange: (v: boolean) => void }) {
  return (
    <label className="switch" onClick={(e) => e.stopPropagation()}>
      <input type="checkbox" checked={!!props.value} onChange={(e) => props.onChange(e.target.checked)} />
      <span className="slider" />
    </label>
  );
}
/** 布尔字段：标签在上、整条可点的开/关切换条在下，与输入框同高对齐 */
function SwitchField(props: { label: string; value: any; onChange: (v: boolean) => void; hint?: string }) {
  const { t } = useLang();
  const on = !!props.value;
  return (
    <div className="field">
      <label>
        {props.label} {props.hint ? <span className="badge">{props.hint}</span> : null}
      </label>
      <button type="button" className={`toggle${on ? ' on' : ''}`} onClick={() => props.onChange(!on)}>
        <Switch value={on} onChange={props.onChange} />
        <span className="state">{on ? t('on') : t('off')}</span>
      </button>
    </div>
  );
}
/** 开关行：文字在左、药丸式圆滑块在右，替代原生 checkbox */
function ToggleRow(props: { label: string; value: boolean; onChange: (v: boolean) => void; className?: string }) {
  return (
    <label className={`checks togglerow${props.className ? ' ' + props.className : ''}`} onClick={(e) => e.stopPropagation()}>
      <span className="tl">{props.label}</span>
      <Switch value={props.value} onChange={props.onChange} />
    </label>
  );
}
/** 三态文本：不动=保持原值；输入=设置；清空/✕=发 null（后端回默认/清空） */
function TriText(props: { value: any; onChange: (v: string | null) => void; placeholder?: string }) {
  const { t } = useLang();
  return (
    <div className="row" style={{ gap: 4 }}>
      <input
        style={{ flex: 1, minWidth: 0 }}
        value={props.value ?? ''}
        placeholder={props.placeholder}
        onChange={(e) => props.onChange(e.target.value === '' ? null : e.target.value)}
      />
      <button type="button" className="btn" title={t('tri.clear1')} onClick={() => props.onChange(null)}>✕</button>
    </div>
  );
}
/** 三态下拉：不动=保持原值；选择=设置；✕=发 null 清空 */
function TriSelect(props: { value: any; options: (string | { label: string; value: string })[]; onChange: (v: string | null) => void; hint?: string }) {
  const { t } = useLang();
  const labelOf = (o: string | { label: string; value: string }) => (typeof o === 'string' ? o : o.label);
  const valOf = (o: string | { label: string; value: string }) => (typeof o === 'string' ? o : o.value);
  return (
    <div className="row" style={{ gap: 4 }}>
      <select
        style={{ flex: 1, minWidth: 0 }}
        value={props.value ?? ''}
        onChange={(e) => {
          if (e.target.value) props.onChange(e.target.value);
        }}
      >
        <option value="">{t('keep')}</option>
        {props.options.map((o) => (
          <option key={valOf(o)} value={valOf(o)}>{labelOf(o)}</option>
        ))}
      </select>
      <button type="button" className="btn" title={t('tri.clear2')} onClick={() => props.onChange(null)}>✕</button>
    </div>
  );
}
/** 键值对行编辑器：obj 行 {k1,k2} 或 arr 行 [v1,v2]。末尾一行为未提交行，输入非空后失焦/点添加提交。 */
function PairRows(props: {
  rows: any[] | undefined;
  kind: 'obj' | 'arr';
  k1: string;
  k2: string;
  ph1: string;
  ph2: string;
  onChange: (rows: any[]) => void;
}) {
  const { t } = useLang();
  const list = Array.isArray(props.rows) ? props.rows : [];
  const [p1, setP1] = useState('');
  const [p2, setP2] = useState('');
  const commit = () => {
    if (!p1.trim() && !p2.trim()) return;
    const row = props.kind === 'obj' ? { [props.k1]: p1.trim(), [props.k2]: p2.trim() } : [p1.trim(), p2.trim()];
    props.onChange([...list, row]);
    setP1('');
    setP2('');
  };
  return (
    <div className="pair-rows">
      {list.map((r, i) => {
        const v1 = props.kind === 'obj' ? r[props.k1] : r[0];
        const v2 = props.kind === 'obj' ? r[props.k2] : r[1];
        return (
          <div className="row" key={i} style={{ gap: 4 }}>
            <input
              style={{ flex: 1, minWidth: 0 }}
              value={v1 ?? ''}
              placeholder={props.ph1}
              onChange={(e) => {
                const c = [...list];
                c[i] = props.kind === 'obj' ? { ...r, [props.k1]: e.target.value } : [e.target.value, r[1]];
                props.onChange(c);
              }}
            />
            <input
              style={{ flex: 1, minWidth: 0 }}
              value={v2 ?? ''}
              placeholder={props.ph2}
              onChange={(e) => {
                const c = [...list];
                c[i] = props.kind === 'obj' ? { ...r, [props.k2]: e.target.value } : [r[0], e.target.value];
                props.onChange(c);
              }}
            />
            <button type="button" className="btn" onClick={() => props.onChange(list.filter((_, j) => j !== i))}>{t('remove')}</button>
          </div>
        );
      })}
      <div className="row" style={{ gap: 4 }}>
        <input style={{ flex: 1, minWidth: 0 }} value={p1} placeholder={props.ph1} onChange={(e) => setP1(e.target.value)} onBlur={commit} />
        <input style={{ flex: 1, minWidth: 0 }} value={p2} placeholder={props.ph2} onChange={(e) => setP2(e.target.value)} onBlur={commit} />
        <button type="button" className="btn" onClick={commit}>{t('add')}</button>
      </div>
    </div>
  );
}
/** 逗号分隔列表：输入期间保留原始字符串草稿（逗号/空格/未完成片段不被即时清洗），
 * 失焦或回车时才按逗号切分、去空白、过滤空项回写数组 */
function CommaField(props: { value: any; onChange: (v: string[]) => void; placeholder?: string; norm?: (x: string) => string }) {
  const [draft, setDraft] = useState<string | null>(null);
  const commit = (v: string) => {
    setDraft(null);
    props.onChange(v.split(',').map((x) => (props.norm ? props.norm(x) : x).trim()).filter(Boolean));
  };
  return (
    <input
      type="text"
      value={draft ?? (props.value ?? []).join(',')}
      placeholder={props.placeholder}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={(e) => commit(e.target.value)}
      onKeyDown={(e) => {
        if (e.key === 'Enter') commit((e.target as HTMLInputElement).value);
      }}
    />
  );
}

/** 后端枚举的固定取值（SCREAMING_SNAKE_CASE，见 komf-api-models common.rs） */
const MANGA_DEX_LINKS = ['MANGADEX', 'ANILIST', 'ANIME_PLANET', 'BOOKWALKER_JP', 'MANGA_UPDATES', 'NOVEL_UPDATES', 'KITSU', 'AMAZON', 'EBOOK_JAPAN', 'MY_ANIME_LIST', 'CD_JAPAN', 'RAW', 'ENGLISH_TL'];
const UPDATE_MODES = ['API', 'COMIC_INFO', 'MYLAR_SERIES_JSON'];

/** 下拉多选：固定取值集合（后端枚举），替代逗号输入框。已选项显示 chips，点开勾选。 */
type MultiOption = string | { label: string; value: string };
function MultiSelect(props: { value: any; options: MultiOption[]; onChange: (v: string[]) => void }) {
  const { t } = useLang();
  const [open, setOpen] = useState(false);
  const boxRef = useRef<HTMLDivElement>(null);
  const vals: string[] = props.value ?? [];
  const labelOf = (o: MultiOption) => (typeof o === 'string' ? o : o.label);
  const valOf = (o: MultiOption) => (typeof o === 'string' ? o : o.value);
  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (boxRef.current && !boxRef.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener('mousedown', onDoc);
    return () => document.removeEventListener('mousedown', onDoc);
  }, [open]);
  const toggle = (v: string) => {
    const next = vals.includes(v) ? vals.filter((x: string) => x !== v) : [...vals, v];
    props.onChange(next);
  };
  const chipLabel = (v: string) => {
    const o = props.options.find((x) => valOf(x) === v);
    return o ? labelOf(o) : v;
  };
  return (
    <div className="multi" ref={boxRef}>
      <button type="button" className={`multi-btn${open ? ' open' : ''}`} onClick={() => setOpen(!open)}>
        {vals.length === 0 ? <span className="muted">{t('none')}</span> : (
          <span className="chips">{vals.map((v) => <span key={v} className="chip">{chipLabel(v)}</span>)}</span>
        )}
        <span className="caret">▾</span>
      </button>
      {open && (
        <div className="multi-menu">
          {props.options.map((o) => (
            <label key={valOf(o)} className={vals.includes(valOf(o)) ? 'sel' : ''}>
              <input type="checkbox" checked={vals.includes(valOf(o))} onChange={() => toggle(valOf(o))} />
              {labelOf(o)}
            </label>
          ))}
        </div>
      )}
    </div>
  );
}
// ---------------- Provider 网格（默认 / 按库覆盖 共用） ----------------

function ProviderList(props: {
  draft: any;
  upd: (path: (string | number)[], value: any) => void;
  base: string[];
  expanded: string | null;
  setExpanded: (k: string | null) => void;
  desc?: string;
}) {
  const { t } = useLang();
  return (
    <div className="card" style={{ background: 'var(--panel2)' }}>
      {props.desc ? <p className="desc">{props.desc}</p> : null}
      {PROVIDER_KEYS.map((key) => {
        const base: (string | number)[] = [...props.base, key];
        const cur = getPath(props.draft, base) ?? {};
        const open = props.expanded === key;
        const upd = props.upd;
        return (
          <div className="card" key={key} style={{ background: 'var(--panel)' }}>
            <div className="provider-head">
              <Switch value={cur.enabled} onChange={(v) => upd([...base, 'enabled'], v)} />
              <b>{PROVIDER_LABEL[key]}</b>
              {PROVIDER_HINT[key] ? <span className="badge">{t(PROVIDER_HINT[key])}</span> : null}
              <span style={{ flex: 1 }} />
              <span className="prio">{t('priority')}
                <input type="number" value={cur.priority ?? 10} onChange={(e) => upd([...base, 'priority'], Number(e.target.value))} />
                <button className="btn" onClick={() => upd([...base, 'priority'], (cur.priority ?? 10) - 10)}>▲</button>
                <button className="btn" onClick={() => upd([...base, 'priority'], (cur.priority ?? 10) + 10)}>▼</button>
                <button className="btn" onClick={() => props.setExpanded(open ? null : key)}>{open ? t('collapse') : t('expand')}</button>
              </span>
            </div>
            {open && (
              <>
                <div className="grid">
                  <Field label={t('f.mediaType')}>
                    <select value={cur.mediaType ?? 'MANGA'} onChange={(e) => upd([...base, 'mediaType'], e.target.value)}>
                      {['MANGA', 'NOVEL', 'COMIC', 'WEBTOON'].map((m) => <option key={m} value={m}>{m}</option>)}
                    </select>
                  </Field>
                  <Field label={t('f.provNameMatchingMode')}>
                    <TriSelect value={cur.nameMatchingMode} options={NAME_MODES.map((m) => ({ label: t('mode.' + m), value: m }))} onChange={(v) => upd([...base, 'nameMatchingMode'], v)} />
                  </Field>
                  <Field label={t('f.authorRoles')}>
                    <MultiSelect value={cur.authorRoles} options={AUTHOR_ROLES.map((r) => ({ label: t('role.' + r), value: r }))} onChange={(v) => upd([...base, 'authorRoles'], v)} />
                  </Field>
                  <Field label={t('f.artistRoles')}>
                    <MultiSelect value={cur.artistRoles} options={AUTHOR_ROLES.map((r) => ({ label: t('role.' + r), value: r }))} onChange={(v) => upd([...base, 'artistRoles'], v)} />
                  </Field>
                  <Field label={t('f.tagWhitelistFile')}>
                    <Text value={cur.tagWhitelistFile} onChange={(v) => upd([...base, 'tagWhitelistFile'], v || null)} placeholder={t('ph.tagWhitelistFile')} />
                  </Field>
                  {key === 'mangaDex' && (
                    <>
                      <Field label={t('f.coverLanguages')}><CommaField value={cur.coverLanguages} onChange={(v) => upd([...base, 'coverLanguages'], v)} placeholder={t('ph.exJaZh')} /></Field>
                      <Field label={t('f.links')}>
                        <MultiSelect value={cur.links} options={MANGA_DEX_LINKS} onChange={(v) => upd([...base, 'links'], v)} />
                      </Field>
                    </>
                  )}
                  {key === 'aniList' && (
                    <>
                      <Field label={t('f.tagsScoreThreshold')}><input type="number" value={cur.tagsScoreThreshold ?? 60} onChange={(e) => upd([...base, 'tagsScoreThreshold'], Number(e.target.value))} /></Field>
                      <Field label={t('f.tagsSizeLimit')}><input type="number" value={cur.tagsSizeLimit ?? 15} onChange={(e) => upd([...base, 'tagsSizeLimit'], Number(e.target.value))} /></Field>
                      <Field label={t('f.titleLanguagePriority')}><CommaField value={cur.titleLanguagePriority} onChange={(v) => upd([...base, 'titleLanguagePriority'], v)} placeholder={t('ph.titleLanguagePriority')} /></Field>
                    </>
                  )}
                  {key === 'mangaBaka' && (
                    <>
                      <Field label={t('f.mode')}><select value={cur.mode ?? 'API'} onChange={(e) => upd([...base, 'mode'], e.target.value)}><option value="API">API</option><option value="DATABASE">DATABASE</option></select></Field>
                      <Field label={t('f.coverLanguages')}><CommaField value={cur.coverLanguages} onChange={(v) => upd([...base, 'coverLanguages'], v)} placeholder={t('ph.exJaZh')} /></Field>
                      <Field label={t('f.updateIntervalHours')}><input type="number" min="0" value={cur.updateIntervalHours ?? 24} onChange={(e) => upd([...base, 'updateIntervalHours'], Number(e.target.value))} /></Field>
                    </>
                  )}
                  {key === 'bangumi' && (
                    <>
                      <Field label={t('f.tagWhitelist')}><CommaField value={cur.tagWhitelist} onChange={(v) => upd([...base, 'tagWhitelist'], v)} /></Field>
                      <SwitchField label={t('f.archiveEnabled')} value={cur.archive?.enabled} onChange={(v) => upd([...base, 'archive', 'enabled'], v)} />
                      <Field label={t('f.archiveDir')}><TriText value={cur.archive?.dir} onChange={(v) => upd([...base, 'archive', 'dir'], v)} /></Field>
                      <Field label={t('f.archiveUpdateIntervalHours')}><input type="number" min="0" value={cur.archive?.updateIntervalHours ?? 168} onChange={(e) => upd([...base, 'archive', 'updateIntervalHours'], Number(e.target.value))} /></Field>
                      <Field label={t('f.archiveIdleReleaseSecs')}><input type="number" min="0" value={cur.archive?.idleReleaseSecs ?? 60} onChange={(e) => upd([...base, 'archive', 'idleReleaseSecs'], Number(e.target.value))} /></Field>
                      <SwitchField label={t('f.staffChineseNames')} value={cur.archive?.staffChineseNames} onChange={(v) => upd([...base, 'archive', 'staffChineseNames'], v)} />
                    </>
                  )}
                  {key === 'bookWalker' && (
                    <Field label={t('f.updateIntervalHours')}><input type="number" min="0" value={cur.updateIntervalHours ?? 24} onChange={(e) => upd([...base, 'updateIntervalHours'], Number(e.target.value))} /></Field>
                  )}
                  {key === 'eHentai' && (
                    <>
                      <Field label={t('f.searchDomain')}><select value={cur.searchDomain ?? 'e-hentai'} onChange={(e) => upd([...base, 'searchDomain'], e.target.value)}><option value="e-hentai">e-hentai</option><option value="exhentai">exhentai</option></select></Field>
                      <Field label={t('f.titlePriority')}><select value={cur.titlePriority ?? 'jpn'} onChange={(e) => upd([...base, 'titlePriority'], e.target.value)}><option value="jpn">jpn</option><option value="title">title</option></select></Field>
                      <SwitchField label={t('f.gidOnlyMatch')} value={cur.gidOnlyMatch} onChange={(v) => upd([...base, 'gidOnlyMatch'], v)} />
                      <Field label={t('f.preferredLanguages')}><CommaField value={cur.preferredLanguages} onChange={(v) => upd([...base, 'preferredLanguages'], v)} /></Field>
                      <Field label={t('f.tagWhitelist')}><CommaField value={cur.tagWhitelist} onChange={(v) => upd([...base, 'tagWhitelist'], v)} /></Field>
                      <Field label={t('f.maleOnlyTagsFile')}><TriText value={cur.maleOnlyTagsFile} onChange={(v) => upd([...base, 'maleOnlyTagsFile'], v)} /></Field>
                      <Field label={t('f.ipbMemberId')} hint={t('sensitive')}><Text password value={cur.ipbMemberId?.includes('*') ? '' : (cur.ipbMemberId ?? '')} onChange={(v) => upd([...base, 'ipbMemberId'], v)} placeholder="********" /></Field>
                      <Field label={t('f.ipbPassHash')} hint={t('sensitive')}><Text password value={cur.ipbPassHash?.includes('*') ? '' : (cur.ipbPassHash ?? '')} onChange={(v) => upd([...base, 'ipbPassHash'], v)} placeholder="********" /></Field>
                      <SwitchField label={t('f.archiveEnabled')} value={cur.archive?.enabled} onChange={(v) => upd([...base, 'archive', 'enabled'], v)} />
                      <Field label={t('f.archiveUrl')}><TriText value={cur.archive?.url} onChange={(v) => upd([...base, 'archive', 'url'], v)} /></Field>
                      <Field label={t('f.archiveDbFile')}><TriText value={cur.archive?.dbFile} onChange={(v) => upd([...base, 'archive', 'dbFile'], v)} /></Field>
                      <Field label={t('f.archiveUpdateIntervalHours')}><input type="number" min="0" value={cur.archive?.updateIntervalHours ?? 4} onChange={(e) => upd([...base, 'archive', 'updateIntervalHours'], Number(e.target.value))} /></Field>
                      <Field label={t('f.archiveIdleReleaseSecs')}><input type="number" min="0" value={cur.archive?.idleReleaseSecs ?? 60} onChange={(e) => upd([...base, 'archive', 'idleReleaseSecs'], Number(e.target.value))} /></Field>
                      <Field label={t('f.archiveSearchCategoryFilter')}><CommaField value={cur.archive?.searchCategoryFilter} onChange={(v) => upd([...base, 'archive', 'searchCategoryFilter'], v)} /></Field>
                      <Field label={t('f.archiveSearchUploaderFilter')}><CommaField value={cur.archive?.searchUploaderFilter} onChange={(v) => upd([...base, 'archive', 'searchUploaderFilter'], v)} /></Field>
                      <SwitchField label={t('f.tagTranslationEnabled')} value={cur.tagTranslationEnabled} onChange={(v) => upd([...base, 'tagTranslationEnabled'], v)} />
                      <Field label={t('f.tagTranslationUrl')}><Text value={cur.tagTranslationUrl} onChange={(v) => upd([...base, 'tagTranslationUrl'], v || null)} placeholder={t('ph.tagTranslationUrl')} /></Field>
                      <Field label={t('f.titleTemplate')}><Text value={cur.titleTemplate} onChange={(v) => upd([...base, 'titleTemplate'], v || null)} /></Field>
                      <Field label={t('f.translatorKeywords')}><CommaField value={cur.translatorKeywords} onChange={(v) => upd([...base, 'translatorKeywords'], v)} /></Field>
                    </>
                  )}
                </div>
                <div className="row"><b>seriesMetadata</b><span className="badge">{t('badge.seriesMetaDefault')}</span></div>
                <div className="checks">
                  {SERIES_FIELDS.map((f) => {
                    // score/useOriginalPublisher/thumbnail 后端默认 false，其余默认 true
                    const on = cur.seriesMetadata?.[f] ?? !['score', 'useOriginalPublisher', 'thumbnail'].includes(f);
                    return (
                      <ToggleRow key={f} label={t('fld.' + f)} value={on} className={on ? '' : 'off'} onChange={(v) => upd([...base, 'seriesMetadata', f], v)} />
                    );
                  })}
                </div>
                <div className="row" style={{ marginTop: 6 }}><b>bookMetadata</b></div>
                <div className="checks">
                  {BOOK_FIELDS.map((f) => {
                    // title/number/numberSort/thumbnail 后端默认 false，其余默认 true
                    const on = cur.bookMetadata?.[f] ?? !['title', 'number', 'numberSort', 'thumbnail'].includes(f);
                    return (
                      <ToggleRow key={f} label={t('fld.' + f)} value={on} className={on ? '' : 'off'} onChange={(v) => upd([...base, 'bookMetadata', f], v)} />
                    );
                  })}
                </div>
              </>
            )}
          </div>
        );
      })}
    </div>
  );
}

// ---------------- 元数据处理表单（default 与按库覆盖共用） ----------------

function MetadataUpdateForm(props: {
  s: string;
  b: string[];
  mu: any;
  draft: any;
  upd: (path: (string | number)[], value: any) => void;
}) {
  const { t } = useLang();
  const { s, b, mu, draft, upd } = props;
  return (
    <>
      <div className="grid">
        <Field label={t('f.libraryType')}><select value={mu.libraryType ?? 'MANGA'} onChange={(e) => upd([s, ...b, 'libraryType'], e.target.value)}>{['MANGA', 'NOVEL', 'COMIC', 'WEBTOON'].map((m) => <option key={m} value={m}>{m}</option>)}</select></Field>
        <SwitchField label={t('f.aggregate')} value={mu.aggregate} onChange={(v) => upd([s, ...b, 'aggregate'], v)} />
        <SwitchField label={t('f.mergeTags')} value={mu.mergeTags} onChange={(v) => upd([s, ...b, 'mergeTags'], v)} />
        <SwitchField label={t('f.mergeGenres')} value={mu.mergeGenres} onChange={(v) => upd([s, ...b, 'mergeGenres'], v)} />
        <SwitchField label={t('f.seriesCovers')} value={mu.seriesCovers} onChange={(v) => upd([s, ...b, 'seriesCovers'], v)} />
        <SwitchField label={t('f.bookCovers')} value={mu.bookCovers} onChange={(v) => upd([s, ...b, 'bookCovers'], v)} />
        <SwitchField label={t('f.overrideExistingCovers')} value={mu.overrideExistingCovers ?? true} onChange={(v) => upd([s, ...b, 'overrideExistingCovers'], v)} />
        <SwitchField label={t('f.lockCovers')} value={mu.lockCovers ?? true} onChange={(v) => upd([s, ...b, 'lockCovers'], v)} />
        <Field label={t('f.updateModes')}><MultiSelect value={mu.updateModes} options={UPDATE_MODES.map((m) => ({ label: t('mode.' + m), value: m }))} onChange={(v) => upd([s, ...b, 'updateModes'], v)} /></Field>
        <SwitchField label={t('f.postSeriesTitle')} value={mu.postProcessing?.seriesTitle} onChange={(v) => upd([s, ...b, 'postProcessing', 'seriesTitle'], v)} />
        <Field label={t('f.seriesTitleLanguage')}><Text value={mu.postProcessing?.seriesTitleLanguage ?? ''} onChange={(v) => upd([s, ...b, 'postProcessing', 'seriesTitleLanguage'], v || null)} placeholder="en/zh/null" /></Field>
        <SwitchField label={t('f.alternativeSeriesTitles')} value={mu.postProcessing?.alternativeSeriesTitles} onChange={(v) => upd([s, ...b, 'postProcessing', 'alternativeSeriesTitles'], v)} />
        <Field label={t('f.altSeriesTitleLangs')}><CommaField value={mu.postProcessing?.alternativeSeriesTitleLanguages} onChange={(v) => upd([s, ...b, 'postProcessing', 'alternativeSeriesTitleLanguages'], v)} placeholder={t('ph.exJaZh')} /></Field>
        <SwitchField label={t('f.fallbackToAltTitle')} value={mu.postProcessing?.fallbackToAltTitle} onChange={(v) => upd([s, ...b, 'postProcessing', 'fallbackToAltTitle'], v)} />
        <SwitchField label={t('f.orderBooks')} value={mu.postProcessing?.orderBooks} onChange={(v) => upd([s, ...b, 'postProcessing', 'orderBooks'], v)} />
        <Field label={t('f.readingDirectionValue')}><select value={mu.postProcessing?.readingDirectionValue ?? ''} onChange={(e) => upd([s, ...b, 'postProcessing', 'readingDirectionValue'], e.target.value || null)}><option value="">null</option><option value="LEFT_TO_RIGHT">LEFT_TO_RIGHT</option><option value="RIGHT_TO_LEFT">RIGHT_TO_LEFT</option><option value="VERTICAL">VERTICAL</option><option value="WEBTOON">WEBTOON</option></select></Field>
        <Field label={t('f.languageValue')}><Text value={mu.postProcessing?.languageValue ?? ''} onChange={(v) => upd([s, ...b, 'postProcessing', 'languageValue'], v || null)} placeholder="null/zh/en" /></Field>
        <Field label={t('f.scoreTagName')}><Text value={mu.postProcessing?.scoreTagName ?? ''} onChange={(v) => upd([s, ...b, 'postProcessing', 'scoreTagName'], v || null)} /></Field>
        <SwitchField label={t('f.searchTitleExtractionEnabled')} value={mu.searchTitleExtraction?.enabled} onChange={(v) => upd([s, ...b, 'searchTitleExtraction', 'enabled'], v)} />
      </div>
      <details className="card-sub">
        <summary>{t('md.advanced')}</summary>
        <div className="grid">
          <SwitchField label={t('f.overrideComicInfo')} value={mu.overrideComicInfo} onChange={(v) => upd([s, ...b, 'overrideComicInfo'], v)} />
          <SwitchField label={t('f.mylarCovers')} value={mu.mylarCovers} onChange={(v) => upd([s, ...b, 'mylarCovers'], v)} />
          <Field label={t('f.mylarOutputDir')}><TriText value={getPath(draft, [s, ...b, 'mylarOutputDir'])} onChange={(v) => upd([s, ...b, 'mylarOutputDir'], v)} placeholder={t('ph.mylarOutputDir')} /></Field>
          <Field label={t('f.failedMatchCollectionName')}><TriText value={getPath(draft, [s, ...b, 'failedMatchCollectionName'])} onChange={(v) => upd([s, ...b, 'failedMatchCollectionName'], v)} placeholder={t('ph.failedMatchCollection')} /></Field>
          <Field label={t('f.originalPublisherTagName')}><TriText value={getPath(draft, [s, ...b, 'postProcessing', 'originalPublisherTagName'])} onChange={(v) => upd([s, ...b, 'postProcessing', 'originalPublisherTagName'], v)} /></Field>
          <SwitchField label={t('f.linksSkipEnabled')} value={mu.postProcessing?.linksSkipEnabled ?? true} onChange={(v) => upd([s, ...b, 'postProcessing', 'linksSkipEnabled'], v)} />
          <SwitchField label={t('f.linksMatchEnabled')} value={mu.postProcessing?.linksMatchEnabled ?? true} onChange={(v) => upd([s, ...b, 'postProcessing', 'linksMatchEnabled'], v)} />
        </div>
        <Field label={t('f.publisherTagNames')} hint={t('hint.langJaEn')}>
          <PairRows
            rows={getPath(draft, [s, ...b, 'postProcessing', 'publisherTagNames'])}
            kind="obj"
            k1="tagName"
            k2="language"
            ph1="tagName"
            ph2="language"
            onChange={(rows) => upd([s, ...b, 'postProcessing', 'publisherTagNames'], rows)}
          />
        </Field>
        <div className="grid grid3">
          <Field label={t('f.altLabelRomaji')}><TriText value={getPath(draft, [s, ...b, 'postProcessing', 'alternateTitleLabels', 'romaji'])} onChange={(v) => upd([s, ...b, 'postProcessing', 'alternateTitleLabels', 'romaji'], v)} /></Field>
          <Field label={t('f.altLabelNative')}><TriText value={getPath(draft, [s, ...b, 'postProcessing', 'alternateTitleLabels', 'native'])} onChange={(v) => upd([s, ...b, 'postProcessing', 'alternateTitleLabels', 'native'], v)} /></Field>
          <Field label={t('f.altLabelLocalized')}><TriText value={getPath(draft, [s, ...b, 'postProcessing', 'alternateTitleLabels', 'localized'])} onChange={(v) => upd([s, ...b, 'postProcessing', 'alternateTitleLabels', 'localized'], v)} /></Field>
        </div>
        <h3 style={{ margin: '10px 0 4px' }}>{t('md.chineseConversion')}</h3>
        <div className="grid">
          <SwitchField label={t('f.ccEnabled')} value={mu.chineseConversion?.enabled} onChange={(v) => upd([s, ...b, 'chineseConversion', 'enabled'], v)} />
          <Field label={t('f.ccDirection')}><select value={mu.chineseConversion?.direction ?? 't2s'} onChange={(e) => upd([s, ...b, 'chineseConversion', 'direction'], e.target.value)}><option value="t2s">t2s</option><option value="s2t">s2t</option></select></Field>
          <SwitchField label={t('md.ccSearch')} value={mu.chineseConversion?.search} onChange={(v) => upd([s, ...b, 'chineseConversion', 'search'], v)} />
          <SwitchField label={t('md.ccMatching')} value={mu.chineseConversion?.matching} onChange={(v) => upd([s, ...b, 'chineseConversion', 'matching'], v)} />
          <SwitchField label="chineseConversion.update（元数据）" value={mu.chineseConversion?.update?.enabled} onChange={(v) => upd([s, ...b, 'chineseConversion', 'update', 'enabled'], v)} />
        </div>
        <Field label={t('md.ccUpdateFields')}>
          <div className="checks">
            {CHINESE_FIELDS.map((f) => {
              const fields = mu.chineseConversion?.update?.fields ?? [];
              const on = fields.includes(f);
              return (
                <ToggleRow key={f} label={t('fld.' + f)} value={on} className={on ? '' : 'off'} onChange={(v) => {
                  const next = v ? [...fields, f] : fields.filter((x: string) => x !== f);
                  upd([s, ...b, 'chineseConversion', 'update', 'fields'], next);
                }} />
              );
            })}
          </div>
        </Field>
        <details className="card-sub">
          <summary>{t('md.searchTitleExtraction')}</summary>
          <div className="grid">
            <Field label={t('f.bracketRegex')}><TriText value={getPath(draft, [s, ...b, 'searchTitleExtraction', 'bracketRegex'])} onChange={(v) => upd([s, ...b, 'searchTitleExtraction', 'bracketRegex'], v)} placeholder={t('ph.bracketRegex')} /></Field>
            <Field label={t('f.authorSeparator')}><TriText value={getPath(draft, [s, ...b, 'searchTitleExtraction', 'authorSeparator'])} onChange={(v) => upd([s, ...b, 'searchTitleExtraction', 'authorSeparator'], v)} /></Field>
            <Field label={t('f.titleSplitters')}><CommaField value={mu.searchTitleExtraction?.titleSplitters} onChange={(v) => upd([s, ...b, 'searchTitleExtraction', 'titleSplitters'], v)} /></Field>
            <Field label={t('f.symbolNormalizeRegex')}><Text value={mu.searchTitleExtraction?.symbolNormalizeRegex ?? ''} onChange={(v) => upd([s, ...b, 'searchTitleExtraction', 'symbolNormalizeRegex'], v || null)} /></Field>
          </div>
          <Field label={t('f.cleanupRegex')}><CommaField value={mu.searchTitleExtraction?.cleanupRegex} onChange={(v) => upd([s, ...b, 'searchTitleExtraction', 'cleanupRegex'], v)} /></Field>
          <Field label={t('f.charMappings')}>
            <PairRows
              rows={mu.searchTitleExtraction?.charMappings}
              kind="arr"
              k1=""
              k2=""
              ph1={t('ph.srcChar')}
              ph2={t('ph.dstChar')}
              onChange={(rows) => upd([s, ...b, 'searchTitleExtraction', 'charMappings'], rows)}
            />
          </Field>
        </details>
      </details>
    </>
  );
}

// ---------------- Jobs 事件流 ----------------

function JobEventsFeed(props: { id: string }) {
  const { t } = useLang();
  const [evs, setEvs] = useState<{ event: string; data: string }[]>([]);
  const [state, setState] = useState<'open' | 'done' | 'error'>('open');
  const [finalJob, setFinalJob] = useState<any>(null);
  useEffect(() => {
    let alive = true;
    const ctl = new AbortController();
    setEvs([]);
    setState('open');
    setFinalJob(null);
    streamJobEvents(props.id, (ev) => {
      if (alive) setEvs((p) => [...p, ev]);
    }, ctl.signal)
      .then(() => {
        if (!alive) return;
        setState('done');
        // 流结束(后端在 Completed 时关流)：再拉一次最终状态兜底
        api.job(props.id).then((j) => { if (alive && j) setFinalJob(j); });
      })
      .catch((e: any) => {
        if (alive) {
          setState('error');
          setEvs((p) => [...p, { event: 'SSEError', data: String(e?.message ?? e) }]);
        }
      });
    return () => {
      alive = false;
      ctl.abort();
    };
  }, [props.id]);

  const badge = (ev: string) =>
    ev === 'ProviderErrorEvent' || ev === 'ProcessingErrorEvent' || ev === 'SSEError'
      ? 'badge bad'
      : ev === 'ProviderCompletedEvent' || ev === 'CompletionEvent'
        ? 'badge ok'
        : ev === 'EventStreamNotFoundEvent'
          ? 'badge warn'
          : 'badge';

  return (
    <div>
      <div className="row" style={{ marginBottom: 8 }}>
        <span className="badge">job {props.id.slice(0, 8)}…</span>
        {state === 'open' ? <span className="badge ok">{t('ev.connecting')}</span> : state === 'done' ? <span className="badge">{t('ev.done')}</span> : <span className="badge bad">{t('ev.error')}</span>}
        {finalJob && (
          <>
            <span className={finalJob.status === 'COMPLETED' ? 'badge ok' : finalJob.status === 'FAILED' ? 'badge bad' : 'badge warn'}>{finalJob.status}</span>
            {finalJob.message ? <span className="badge">{finalJob.message}</span> : null}
          </>
        )}
      </div>
      {evs.length === 0 ? <p className="desc">{t('ev.waiting')}</p> : null}
      <div className="ev-list">
        {evs.map((ev, i) => {
          let d: any = null;
          try { d = JSON.parse(ev.data); } catch { d = ev.data; }
          return (
            <div key={i} className="ev-line">
              <span className={badge(ev.event)}>{ev.event}</span>
              {ev.event === 'ProviderBookEvent' && d && typeof d.totalBooks === 'number' ? (
                <span className="ev-progress">
                  <span className="ev-progress-bar" style={{ width: `${d.totalBooks ? Math.max(3, (d.bookProgress / d.totalBooks) * 100) : 0}%` }} />
                  <span className="ev-progress-text">{d.provider} {d.bookProgress}/{d.totalBooks}</span>
                </span>
              ) : (
                <span className="ev-data">{typeof d === 'string' ? d : JSON.stringify(d)}</span>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}


// ---------------- Tracker（阅读状态同步） ----------------

const TRACKER_PROVIDERS = ['anilist', 'mal', 'bangumi', 'mangabaka'];
const TRACKER_STATUSES = ['', 'reading', 'planning', 'completed', 'paused', 'dropped', 'rereading'];
const TRACKER_STATUS_LABEL: Record<string, string> = {
  reading: 'Reading', planning: 'Planning', completed: 'Completed',
  paused: 'Paused', dropped: 'Dropped', rereading: 'Rereading',
};
const TRACKER_PROVIDER_LABEL: Record<string, string> = { anilist: 'AniList', mal: 'MyAnimeList', bangumi: 'Bangumi', mangabaka: 'MangaBaka' };

function TrackerPage() {
  const { t } = useLang();
  const [provider, setProvider] = useState('anilist');
  const [loggedIn, setLoggedIn] = useState(false);
  const [query, setQuery] = useState('');
  const [busy, setBusy] = useState(false);
  const [results, setResults] = useState<any[] | null>(null);
  const [err, setErr] = useState('');
  const [trackId, setTrackId] = useState('');
  const [state, setState] = useState<any | null>(null);
  const [stateBusy, setStateBusy] = useState(false);
  const [score, setScore] = useState('');
  const [status, setStatus] = useState('');
  const [chapter, setChapter] = useState('');
  const [volume, setVolume] = useState('');
  const [startDate, setStartDate] = useState('');
  const [finishDate, setFinishDate] = useState('');
  const [saved, setSaved] = useState('');
  const [nsfw, setNsfw] = useState(true);
  const [links, setLinks] = useState<any[] | null>(null);
  const [linksBusy, setLinksBusy] = useState(false);
  const [pickedTitle, setPickedTitle] = useState('');
  const [pickedCover, setPickedCover] = useState('');
  const [loginLost, setLoginLost] = useState(false);

  useEffect(() => {
    let alive = true;
    api.oauthStatus(provider).then((s) => {
      if (!alive) return;
      setLoggedIn(s.logged_in);
      if (!s.logged_in) { setResults(null); setState(null); }
    });
    return () => { alive = false; };
  }, [provider]);

  // 401 (token lost) -> also flag login lost for a re-sign-in banner.
  function fail(e: any) {
    const msg = String(e?.message ?? e);
    setErr(msg);
    if (/^401\b/.test(msg)) setLoginLost(true);
  }

  async function doSearch() {
    setBusy(true); setErr(''); setResults(null); setLinks(null); setState(null); setSaved('');
    try {
      // 链接输入由 tracker 后端识别（搜索接口直接返回单条目）。
      const res = await fetch(`/api/tracker/${provider}/search?name=${encodeURIComponent(query)}&nsfw=${nsfw}`);
      if (!res.ok) {
        const b = await res.text().catch(() => '');
        throw new Error(`${res.status}: ${b.slice(0, 200)}`);
      }
      const list = await res.json();
      setResults(list);
      // 输入是平台链接时后端返回单条，自动选中进入状态表单。
      if (/^https?:\/\//i.test(query.trim()) && Array.isArray(list) && list.length === 1) {
        pick(list[0]);
      }
    } catch (e: any) {
      fail(e);
    } finally {
      setBusy(false);
    }
  }

  function pick(r: any) {
    if (trackId === r.id) { setTrackId(''); return; } // 再次点击同一条目：收起
    setTrackId(r.id);
    setPickedTitle(r.title ?? '');
    setPickedCover(r.coverUrl ?? '');
    setSaved('');
    loadState(r.id);
  }

  async function loadLinks() {
    setLinksBusy(true); setErr(''); setResults(null);
    try {
      const res = await fetch('/api/tracker/links');
      if (!res.ok) {
        const b = await res.text().catch(() => '');
        throw new Error(`${res.status}: ${b.slice(0, 200)}`);
      }
      setLinks(await res.json());
    } catch (e: any) {
      fail(e);
    } finally {
      setLinksBusy(false);
    }
  }

  function openLink(l: any) {
    if (trackId === l.trackId) { setTrackId(''); return; } // 再次点击同一条目：收起
    setProvider(l.provider);
    setTrackId(l.trackId);
    setPickedTitle(l.title ?? '');
    setPickedCover(l.coverUrl ?? '');
    setSaved('');
    setResults(null);
    loadState(l.trackId, l.provider);
  }

  async function loadState(id: string, p?: string) {
    setStateBusy(true); setErr('');
    const prov = p ?? provider;
    try {
      const res = await fetch(`/api/tracker/${prov}/state?trackId=${encodeURIComponent(id)}`);
      if (!res.ok) {
        const b = await res.text().catch(() => '');
        throw new Error(`${res.status}: ${b.slice(0, 200)}`);
      }
      const st = await res.json();
      setState(st);
      setScore(st.score ?? '');
      setStatus(st.status ?? '');
      setChapter(st.lastReadChapter ?? '');
      setVolume(st.lastReadVolume ?? '');
      setStartDate(st.startReadDate ?? '');
      setFinishDate(st.finishReadDate ?? '');
    } catch (e: any) {
      fail(e);
    } finally {
      setStateBusy(false);
    }
  }

  async function pushUpdate() {
    setBusy(true); setErr(''); setSaved('');
    const body: any = { trackId };
    if (pickedTitle) body.title = pickedTitle;
    if (pickedCover) body.coverUrl = pickedCover;
    if (status) body.status = status;
    if (score !== '') body.score = Number(score);
    if (chapter !== '') body.lastReadChapter = Number(chapter);
    if (volume !== '') body.lastReadVolume = Number(volume);
    if (startDate) body.startReadDate = startDate;
    if (finishDate) body.finishReadDate = finishDate;
    try {
      const res = await fetch(`/api/tracker/${provider}/update`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
      if (!res.ok) {
        const b = await res.text().catch(() => '');
        throw new Error(`${res.status}: ${b.slice(0, 200)}`);
      }
      setSaved(t('tr.pushed'));
      loadState(trackId);
    } catch (e: any) {
      fail(e);
    } finally {
      setBusy(false);
    }
  }

  function renderStatePanel() {
    return (
      <div className="card" style={{ marginTop: 8, marginBottom: 8 }}>
        <h3>{t('tr.selected', { id: trackId })}</h3>
        <p className="desc">
          {stateBusy ? t('tr.stateLoading') : t('tr.scoreHint')}
        </p>
        {state && !stateBusy ? (
          state.status || state.score != null || state.lastReadChapter != null || state.lastReadVolume != null || state.startReadDate || state.finishReadDate ? (
            <div className="row" style={{ flexWrap: 'wrap', gap: 8 }}>
              <span className="badge">{t('tr.state')}: {state.status ? TRACKER_STATUS_LABEL[state.status] ?? state.status : '-'}</span>
              <span className="badge">{t('tr.score')}: {state.score ?? '-'}</span>
              <span className="badge">{t('tr.chapter')}: {state.lastReadChapter ?? '-'}{state.totalChapters ? ` / ${state.totalChapters}` : ''}</span>
              <span className="badge">{t('tr.volume')}: {state.lastReadVolume ?? '-'}{state.totalVolumes ? ` / ${state.totalVolumes}` : ''}</span>
            </div>
          ) : (
            <div className="toast ok">{t('tr.noEntry')}</div>
          )
        ) : null}
        <div className="grid" style={{ marginTop: 8 }}>
          <div className="fld">
            <label>{t('tr.status')}</label>
            <select value={status} onChange={(e) => setStatus(e.target.value)}>
              {TRACKER_STATUSES.map((s) => <option key={s} value={s}>{s ? TRACKER_STATUS_LABEL[s] : t('keep')}</option>)}
            </select>
          </div>
          <div className="fld">
            <label>{t('tr.score')}</label>
            <input type="number" value={score} onChange={(e) => setScore(e.target.value)} placeholder="0" />
          </div>
          <div className="fld">
            <label>{t('tr.chapter')}</label>
            <input type="number" value={chapter} onChange={(e) => setChapter(e.target.value)} placeholder="0" />
          </div>
          <div className="fld">
            <label>{t('tr.volume')}</label>
            <input type="number" value={volume} onChange={(e) => setVolume(e.target.value)} placeholder="0" />
          </div>
          <div className="fld">
            <label>{t('tr.startDate')}</label>
            <input type="date" value={startDate} onChange={(e) => setStartDate(e.target.value)} />
          </div>
          <div className="fld">
            <label>{t('tr.finishDate')}</label>
            <input type="date" value={finishDate} onChange={(e) => setFinishDate(e.target.value)} />
          </div>
        </div>
        <div className="row" style={{ marginTop: 8 }}>
          <button className="btn primary" disabled={busy} onClick={pushUpdate}>{busy ? t('searching') : t('tr.push')}</button>
          <button className="btn" disabled={stateBusy} onClick={() => loadState(trackId)}>{t('refresh')}</button>
        </div>
      </div>
    );
  }

  return (
    <>
      <div className="card">
        <h2>{t('tr.title')}</h2>
        <p className="desc">{t('tr.desc')}</p>
        <div className="row" style={{ flexWrap: 'wrap', gap: 8 }}>
          <select value={provider} onChange={(e) => { setProvider(e.target.value); setTrackId(''); setLoginLost(false); }}>
            {TRACKER_PROVIDERS.map((p) => <option key={p} value={p}>{TRACKER_PROVIDER_LABEL[p]}</option>)}
          </select>
          {loggedIn ? (
            <span className="badge" style={{ color: 'var(--ok)' }}>{t('tr.loggedIn')}</span>
          ) : (
            <span className="badge">{t('tr.notLoggedIn')}</span>
          )}
          <a className="btn" href={`/api/oauth/${provider}/start`} onClick={() => setTrackId('')}>{t('oauth.login')}</a>
          <button className="btn" onClick={loadLinks} disabled={linksBusy}>{linksBusy ? t('searching') : t('tr.links')}</button>
        </div>
        {loggedIn && (
          <div className="row" style={{ marginTop: 8 }}>
            <input style={{ flex: 1 }} value={query} placeholder={t('tr.phQuery')} onChange={(e) => setQuery(e.target.value)} onKeyDown={(e) => e.key === 'Enter' && doSearch()} />
            <label className="muted" style={{ display: 'inline-flex', alignItems: 'center', gap: 4, fontSize: 12, whiteSpace: 'nowrap' }}>
              <input type="checkbox" checked={nsfw} onChange={(e) => setNsfw(e.target.checked)} />
              {t('tr.nsfw')}
            </label>
            <button className="btn primary" disabled={busy || !query.trim()} onClick={doSearch}>{busy ? t('searching') : t('tr.search')}</button>
          </div>
        )}
      </div>

      {loginLost && (
        <div className="toast err" style={{ display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap' }}>
          <span>{t('tr.loginLost')}</span>
          <a className="btn" href={`/api/oauth/${provider}/start`} onClick={() => setLoginLost(false)}>{t('tr.relogin')}</a>
        </div>
      )}
      {err ? <div className="toast err">{err}</div> : null}
      {saved ? <div className="toast ok">{saved}</div> : null}

      {links && (
        <div className="card" style={{ marginTop: 10 }}>
          <p className="desc">{t('tr.linksDesc')}</p>
          {links.length === 0 ? <div className="toast ok">{t('tr.linksEmpty')}</div> : null}
          {links.map((l, i) => (
            <Fragment key={i}>
              <div className="search-hit" style={{ cursor: 'pointer' }} onClick={() => openLink(l)}>
                {l.coverUrl ? (
                  <CoverImg key={`${l.provider}-${l.trackId}-${l.coverUrl}`} url={l.coverUrl} className="hit-cover" />
                ) : (
                  <div className="hit-cover empty" style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', fontSize: 11 }}>
                    {TRACKER_PROVIDER_LABEL[l.provider] ?? l.provider}
                  </div>
                )}
                <div className="hit-body">
                  <b>{l.title ?? l.trackId}</b>
                  <div className="row">
                    <span className="badge">{l.provider}</span>
                    {l.url ? <a href={l.url} target="_blank" rel="noreferrer">{t('source')}</a> : null}
                    <span className="muted" style={{ fontSize: 12 }}>{l.updatedAt ? new Date(l.updatedAt * 1000).toLocaleString() : ''}</span>
                  </div>
                </div>
                <span className="btn">{t('tr.pick')}</span>
              </div>
              {l.trackId === trackId && renderStatePanel()}
            </Fragment>
          ))}
        </div>
      )}

      {results && (
        <div className="card" style={{ marginTop: 10 }}>
          <p className="desc">{t('tr.results', { n: results.length })}</p>
          {results.length === 0 ? <div className="toast ok">{t('noResults')}</div> : null}
          {results.map((r, i) => (
            <Fragment key={i}>
              <div className="search-hit">
                {r.coverUrl ? (
                  <CoverImg key={`${r.id}-${r.coverUrl}`} url={r.coverUrl} className="hit-cover" />
                ) : <div className="hit-cover empty" />}
                <div className="hit-body">
                  <b>{r.title}</b>
                  <div className="row">
                    {r.tracked ? <span className="badge">{t('tr.tracked')}</span> : null}
                    {r.url ? <a href={r.url} target="_blank" rel="noreferrer">{t('source')}</a> : null}
                  </div>
                  {r.description ? <p className="muted" style={{ fontSize: 12, margin: '4px 0 0' }}>{r.description.slice(0, 200)}{r.description.length > 200 ? '…' : ''}</p> : null}
                </div>
                <button className="btn" onClick={() => pick(r)}>{t('tr.pick')}</button>
              </div>
              {r.id === trackId && renderStatePanel()}
            </Fragment>
          ))}
        </div>
      )}
    </>
  );
}

// ---------------- 主应用 ----------------

export default function App() {
  const { t, lang, setLang } = useLang();
  const [tab, setTab] = useState('overview');
  const [theme, setTheme] = useState<'system' | 'light' | 'dark'>(() => {
    const savedTheme = localStorage.getItem('komf-theme');
    return savedTheme === 'light' || savedTheme === 'dark' || savedTheme === 'system' ? savedTheme : 'system';
  });
  useEffect(() => {
    const el = document.documentElement;
    if (theme === 'system') delete el.dataset.theme;
    else el.dataset.theme = theme;
    try { localStorage.setItem('komf-theme', theme); } catch { /* ignore */ }
  }, [theme]);
  const [orig, setOrig] = useState<any>(null);
  const [draft, setDraft] = useState<any>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);
  const [version, setVersion] = useState('');
  const [conn, setConn] = useState<Record<string, any>>({});
  const [libs, setLibs] = useState<Record<string, any[]>>({});
  const [expanded, setExpanded] = useState<string | null>(null);
  const [dbBusy, setDbBusy] = useState<string | null>(null);
  const [dbProg, setDbProg] = useState<{ kind: string; pct: number; info?: string } | null>(null);
  const [dbErr, setDbErr] = useState<{ kind: string; msg: string } | null>(null);
  // OAuth 登录态（anilist / mal / bangumi / mangabaka，共享 client + 中转页）
  const OAUTH_PROVIDERS = ['anilist', 'mal', 'bangumi', 'mangabaka'];
  const [oauth, setOauth] = useState<Record<string, { logged_in: boolean; username?: string | null }>>({});
  const [oauthBusy, setOauthBusy] = useState(false);
  // 按库覆盖编辑
  const [scopeServer, setScopeServer] = useState<ServerKind>('komga');
  const [scopeLib, setScopeLib] = useState('');
  // 搜索试跑
  const [searchServer, setSearchServer] = useState<ServerKind>('komga');
  const [searchLib, setSearchLib] = useState('');
  const [searchName, setSearchName] = useState('');
  const [searchSeriesId, setSearchSeriesId] = useState('');
  const [searchResult, setSearchResult] = useState<any>(null);
  const [searchErr, setSearchErr] = useState('');
  const [searchBusy, setSearchBusy] = useState(false);
  // 通知模板
  const [dTpl, setDTpl] = useState<any>(null);
  const [dTplEdit, setDTplEdit] = useState<any>(null);
  const [aTpl, setATpl] = useState<any>(null);
  const [aTplEdit, setATplEdit] = useState<any>(null);
  const [notifCtx, setNotifCtx] = useState(() => JSON.stringify(SAMPLE_CTX, null, 2));
  const [renderOut, setRenderOut] = useState<any>(null);
  const [notifMsg, setNotifMsg] = useState<{ ok: boolean; text: string } | null>(null);
  const [notifBusy, setNotifBusy] = useState<string | null>(null);
  // Jobs
  const [jobServer, setJobServer] = useState<ServerKind>('komga');
  const [jobLib, setJobLib] = useState('');
  const [jobSeriesId, setJobSeriesId] = useState('');
  const [removeComicInfo, setRemoveComicInfo] = useState(false);
  const [jobs, setJobs] = useState<any[]>([]);
  const [jobStatus, setJobStatus] = useState('');
  const [activeJobId, setActiveJobId] = useState('');

  const upd = (path: (string | number)[], value: any) => {
    setDraft((d: any) => {
      const c = clone(d);
      setPath(c, path, value);
      return c;
    });
  };

  /** 触发 MangaBaka/BookWalker/Bangumi/EHentai 离线 DB 下载（jsonl 进度流 → 进度条；完成后重载刷新徽标） */
  async function runDbDownload(kind: DbKind) {
    if (dbBusy) return;
    setDbBusy(kind);
    setDbErr(null);
    setDbProg({ kind, pct: 0 });
    try {
      await updateDb(kind, (ev) => {
        if (ev.type === 'ProgressEvent') {
          const pct = ev.total && ev.total > 0 ? Math.min(100, Math.round(((ev.completed ?? 0) / ev.total) * 100)) : 0;
          setDbProg({ kind, pct, info: ev.info ?? undefined });
        } else if (ev.type === 'ErrorEvent') {
          setDbErr({ kind, msg: ev.message ?? t('db.failed') });
        } else if (ev.type === 'FinishedEvent') {
          setDbProg({ kind, pct: 100 });
        }
      });
      await load();
    } catch (e: any) {
      setDbErr({ kind, msg: e.message });
    } finally {
      setDbBusy(null);
    }
  }

  async function load() {
    setLoading(true);
    setMsg(null);
    // OAuth 回调跳回（/?oauth=success|error）时显示结果提示
    try {
      const q = new URLSearchParams(window.location.search);
      if (q.get('oauth') === 'success') setMsg({ ok: true, text: t('oauth.success') });
      else if (q.get('oauth') === 'error') setMsg({ ok: false, text: t('oauth.failed', { msg: q.get('message') ?? '' }) });
    } catch { /* ignore */ }
    refreshOAuth();
    try {
      const [cfg, ver] = await Promise.all([api.getConfig(), api.version().catch(() => ({ version: '' }) as any)]);
      setOrig(cfg);
      setDraft(clone(cfg));
      setVersion((ver as any)?.version ?? '');
      const c: Record<string, any> = {};
      const l: Record<string, any[]> = {};
      await Promise.all(
        SERVERS.map(async (s) => {
          c[s] = await api.connected(s).catch((e) => ({ success: false, errorMessage: String(e) }));
          l[s] = await api.libraries(s);
        }),
      );
      setConn(c);
      setLibs(l);
      // 通知模板与任务列表一并拉取
      const [dt, at, jp] = await Promise.all([
        api.getDiscordTemplates().catch(() => null),
        api.getAppriseTemplates().catch(() => null),
        api.jobs(),
      ]);
      if (dt) { setDTpl(dt); setDTplEdit(clone(dt)); }
      if (at) { setATpl(at); setATplEdit(clone(at)); }
      setJobs(jp.content);
    } catch (e: any) {
      if (String(e?.message ?? '').includes('401')) { window.location.href = '/login'; return; }
      setMsg({ ok: false, text: t('err.loadFailed', { msg: e.message }) });
    } finally {
      setLoading(false);
    }
  }
  useEffect(() => {
    load();
  }, []);

  /** 拉取三平台 OAuth 登录态（登录/退出后重拉刷新徽标） */
  async function refreshOAuth() {
    setOauthBusy(true);
    try {
      const entries = await Promise.all(OAUTH_PROVIDERS.map(async (p) => [p, await api.oauthStatus(p)] as const));
      setOauth(Object.fromEntries(entries));
    } catch { /* 单个失败按未登录处理 */ } finally {
      setOauthBusy(false);
    }
  }

  /** 退出某平台 OAuth 登录态 */
  async function oauthLogout(p: string) {
    try {
      await api.oauthLogout(p);
      setMsg({ ok: true, text: t('oauth.logout') });
    } catch (e: any) {
      setMsg({ ok: false, text: t('err.loadFailed', { msg: e.message }) });
    }
    refreshOAuth();
  }

  const patch = useMemo(() => {
    if (!orig || !draft) return undefined;
    return diff(orig, draft);
  }, [orig, draft]);
  const dirty = !isEmptyPatch(patch);

  async function save() {
    setSaving(true);
    setMsg(null);
    try {
      let body: any = clone(patch) ?? {};
      // 按库覆盖现在与 default 共用同一套表单组件，变更直接写入 draft，
      // 由 diff(orig, draft) 生成 patch（含该库 metadataUpdate 覆盖差异）；
      // 「删除覆盖」按钮将库覆盖置 null，patch 发送 null 即删除。
      if (isEmptyPatch(body)) {
        setMsg({ ok: true, text: t('save.noChange') });
        return;
      }
      await api.patchConfig(body);
      setMsg({ ok: true, text: t('save.done') });
      await load();
    } catch (e: any) {
      setMsg({ ok: false, text: e.message });
    } finally {
      setSaving(false);
    }
  }


  // ---- Jobs 触发 / 刷新 ----
  async function refreshJobs() {
    try {
      const page = await api.jobs(jobStatus || undefined);
      setJobs(page.content);
    } catch (e: any) {
      setMsg({ ok: false, text: t('err.jobs', { msg: e.message }) });
    }
  }
  useEffect(() => {
    if (tab === 'jobs') refreshJobs();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tab]);

  async function runTrigger(kind: 'matchSeries' | 'matchLib' | 'resetSeries' | 'resetLib') {
    setMsg(null);
    try {
      if (!jobLib) { setMsg({ ok: false, text: t('jobs.needLib') }); return; }
      if ((kind === 'matchSeries' || kind === 'resetSeries') && !jobSeriesId.trim()) {
        setMsg({ ok: false, text: t('jobs.needSeriesId') });
        return;
      }
      if (kind === 'matchSeries') {
        const id = await api.matchSeries(jobServer, jobLib, jobSeriesId.trim());
        setActiveJobId(id);
        setMsg({ ok: true, text: t('jobs.seriesTriggered', { id: id.slice(0, 8) }) });
      } else if (kind === 'matchLib') {
        await api.matchLibrary(jobServer, jobLib);
        setMsg({ ok: true, text: t('jobs.libTriggered') });
        setTimeout(refreshJobs, 3000);
      } else if (kind === 'resetSeries') {
        await api.resetSeries(jobServer, jobLib, jobSeriesId.trim(), removeComicInfo);
        setMsg({ ok: true, text: t('jobs.seriesReset') });
        refreshJobs();
      } else {
        await api.resetLibrary(jobServer, jobLib, removeComicInfo);
        setMsg({ ok: true, text: t('jobs.libReset') });
        refreshJobs();
      }
    } catch (e: any) {
      setMsg({ ok: false, text: e.message });
    }
  }

  async function clearJobs() {
    if (!window.confirm(t('jobs.clearConfirm'))) return;
    try {
      await api.deleteJobs();
      setJobs([]);
      setMsg({ ok: true, text: t('jobs.cleared') });
    } catch (e: any) {
      setMsg({ ok: false, text: t('err.clear', { msg: e.message }) });
    }
  }

  // ---- 搜索试跑：一键设元数据 ----
  async function doSearch() {
    setSearchErr('');
    setSearchResult(null);
    if (!searchName.trim()) return;
    setSearchBusy(true);
    try {
      setSearchResult(await api.search(searchServer, searchName.trim(), searchLib || undefined, searchSeriesId.trim() || undefined));
    } catch (e: any) {
      setSearchErr(e.message);
    } finally {
      setSearchBusy(false);
    }
  }
  async function applyFromSearch(r: any) {
    setSearchErr('');
    try {
      const id = await api.identify(searchServer, {
        libraryId: searchLib || undefined,
        seriesId: searchSeriesId.trim(),
        provider: r.provider,
        providerSeriesId: r.result_id,
      });
      setActiveJobId(id);
      setTab('Jobs');
      setMsg({ ok: true, text: t('jobs.identify', { id: id.slice(0, 8) }) });
    } catch (e: any) {
      setSearchErr(String(e.message));
    }
  }

  // ---- 通知模板 / 渲染 / 发送 ----
  const setDField = (i: number, k: string, v: any) =>
    setDTplEdit((t: any) => ({ ...t, fields: (t?.fields ?? []).map((f: any, j: number) => (j === i ? { ...f, [k]: v } : f)) }));
  async function notifAction(ch: 'discord' | 'apprise', act: 'save' | 'render' | 'send') {
    setNotifBusy(`${ch}-${act}`);
    setNotifMsg(null);
    setRenderOut(null);
    let ctx: any;
    try {
      ctx = JSON.parse(notifCtx);
    } catch (e: any) {
      setNotifMsg({ ok: false, text: t('err.ctxJson', { msg: e.message }) });
      setNotifBusy(null);
      return;
    }
    try {
      if (act === 'save') {
        if (ch === 'discord') {
          await api.updateDiscordTemplates(dTplEdit);
          const fresh = await api.getDiscordTemplates();
          setDTpl(fresh);
          setDTplEdit(clone(fresh));
        } else {
          await api.updateAppriseTemplates(aTplEdit);
          const fresh = await api.getAppriseTemplates();
          setATpl(fresh);
          setATplEdit(clone(fresh));
        }
        setNotifMsg({ ok: true, text: t('notif.saved', { ch: ch === 'discord' ? 'Discord' : 'Apprise' }) });
      } else if (act === 'render') {
        const body = { context: ctx, templates: ch === 'discord' ? dTplEdit : aTplEdit };
        const r = ch === 'discord' ? await api.renderDiscord(body) : await api.renderApprise(body);
        setRenderOut({ channel: ch, data: r });
      } else {
        const body = { context: ctx, templates: ch === 'discord' ? dTplEdit : aTplEdit };
        if (ch === 'discord') await api.sendDiscord(body);
        else await api.sendApprise(body);
        setNotifMsg({ ok: true, text: t('notif.sent', { ch: ch === 'discord' ? 'Discord' : 'Apprise' }) });
      }
    } catch (e: any) {
      setNotifMsg({ ok: false, text: e.message });
    } finally {
      setNotifBusy(null);
    }
  }

  if (loading || !draft) return <div className="main">{t('loading')}</div>;

  const dp = draft.metadataProviders?.defaultProviders ?? {};
  const enabledCount = PROVIDER_KEYS.filter((p) => dp[p]?.enabled).length;
  const libOptions = (libs[scopeServer] ?? []).map((l: any) => ({ id: l.id, name: l.name ?? l.id }));
  const scopeHasProviders = !!scopeLib;

  return (
    <>
      <div className="topbar">
        <h1>{t('app.title')}</h1>
        <span className="ver">{version ? `v${version}` : ''}</span>
        {dirty ? <span className="badge warn">{t('sync.dirty')}</span> : <span className="badge">{t('sync.clean')}</span>}
        <span style={{ flex: 1 }} />
        <button className="btn" title={theme === 'system' ? t('theme.title.system') : theme === 'light' ? t('theme.title.light') : t('theme.title.dark')} onClick={() => setTheme(theme === 'system' ? 'light' : theme === 'light' ? 'dark' : 'system')}>{theme === 'system' ? '🌓' : theme === 'light' ? '☀️' : '🌙'}</button>
        <button className="btn" title={lang === 'zh' ? t('lang.title.zh') : t('lang.title.en')} onClick={() => setLang(lang === 'zh' ? 'en' : 'zh')}>{lang === 'zh' ? t('lang.cur.zh') : t('lang.cur.en')}</button>
        <button className="btn" onClick={load} disabled={saving}>{t('reload')}</button>
        <button className="btn primary" onClick={save} disabled={saving || (!dirty && !scopeLib)}>
          {saving ? t('saving') : t('savePatch')}
        </button>
      </div>
      {msg ? <div className="main" style={{ paddingBottom: 0 }}><div className={`toast ${msg.ok ? 'ok' : 'err'}`}>{msg.text}</div></div> : null}
      <div className="tabs">
        {TABS.map((tb) => (
          <button key={tb} className={tab === tb ? 'active' : ''} onClick={() => setTab(tb)}>{t('tab.' + tb)}</button>
        ))}
      </div>
      <div className="main">
        {tab === 'tracker' && <TrackerPage />}
        {tab === 'overview' && (
          <>
            <div className="card">
              <h2>{t('ov.status')}</h2>
              <p className="desc">{t('ov.connDesc', { servers: '{komga,kavita,stump}' })}</p>
              <div className="grid">
                {SERVERS.map((s) => (
                  <div key={s} className="card" style={{ margin: 0 }}>
                    <div className="row">
                      <span className="dot" style={{ background: conn[s]?.success ? 'var(--ok)' : 'var(--bad)' }} />
                      <b>{SERVER_LABEL[s]}</b>
                      <span className="badge">{getPath(draft, [s, 'baseUri']) ?? t('unset')}</span>
                    </div>
                    <div style={{ color: 'var(--muted)', fontSize: 12 }}>
                      {conn[s]?.success ? t('ov.connected', { n: libs[s]?.length ?? 0, ies: libs[s]?.length === 1 ? 'y' : 'ies' }) : conn[s]?.errorMessage ?? t('ov.disconnected')}
                    </div>
                    <div style={{ fontSize: 12, overflowWrap: 'anywhere' }}>{(libs[s] ?? []).map((l: any) => l.name ?? l.id).join(' / ')}</div>
                  </div>
                ))}
              </div>
              <div className="row" style={{ marginTop: 8 }}>
                <span className="badge">{t('ov.enabledProviders', { a: enabledCount, b: PROVIDER_KEYS.length })}</span>
                <span className="badge">{t('ov.nameMatchingMode')}: {draft.metadataProviders?.nameMatchingMode ? t('mode.' + draft.metadataProviders.nameMatchingMode) : '-'}</span>
                <span className="badge">{t('ov.mangaBakaDb', { v: draft.metadataProviders?.mangaBakaDatabase?.downloadTimestamp ?? t('db.undownloaded') })}</span>
                <span className="badge">{t('ov.bookWalkerDb', { v: draft.metadataProviders?.bookWalkerDownloadDate ?? t('db.undownloaded') })}</span>
                <span className="badge">{t('ov.bangumiDb', { v: draft.metadataProviders?.bangumiDatabase?.downloadTimestamp ?? t('db.undownloaded') })}</span>
                <span className="badge">{t('ov.ehentaiDb', { v: draft.metadataProviders?.ehentaiDatabase?.downloadTimestamp ?? t('db.undownloaded') })}</span>
              </div>
            </div>
            <div className="card">
              <h2>{t('ov.search')}</h2>
              <p className="desc">{t('ov.searchDesc')}</p>
              <div className="row">
                <select value={searchServer} onChange={(e) => setSearchServer(e.target.value as ServerKind)}>
                  {SERVERS.map((s) => <option key={s} value={s}>{s}</option>)}
                </select>
                <select value={searchLib} onChange={(e) => setSearchLib(e.target.value)}>
                  <option value="">{t('allLibs')}</option>
                  {(libs[searchServer] ?? []).map((l: any) => <option key={l.id} value={l.id} title={`${l.name} (${l.id})`}>{l.name} ({l.id})</option>)}
                </select>
                <input style={{ flex: 1 }} value={searchName} placeholder={t('ph.seriesName')} onChange={(e) => setSearchName(e.target.value)} onKeyDown={(e) => e.key === 'Enter' && doSearch()} />
                <input style={{ width: 220 }} value={searchSeriesId} placeholder={t('ph.seriesId')} onChange={(e) => setSearchSeriesId(e.target.value)} />
                <button className="btn primary" disabled={searchBusy || !searchName.trim()} onClick={doSearch}>{searchBusy ? t('searching') : t('search')}</button>
              </div>
              {searchErr ? <div className="toast err">{searchErr}</div> : null}
              {Array.isArray(searchResult) && (
                <div style={{ marginTop: 10 }}>
                  <p className="desc">{t('hits', { n: searchResult.length })}</p>
                  {searchResult.length === 0 ? <div className="toast ok">{t('noResults')}</div> : null}
                  {searchResult.map((r, i) => (
                    <div className="search-hit" key={i}>
                      {r.image_url || r.imageUrl ? (
                        <CoverImg url={r.image_url || r.imageUrl} className="hit-cover" />
                      ) : <div className="hit-cover empty" />}
                      <div className="hit-body">
                        <b>{r.title}</b>
                        <div className="row">
                          <span className="badge">{PROVIDER_LABEL[r.provider] ?? r.provider}</span>
                          {r.language ? <span className="badge">{r.language}</span> : null}
                          {r.mediaType ? <span className="badge">{r.mediaType}</span> : null}
                          {r.url ? <a href={r.url} target="_blank" rel="noreferrer">{t('source')}</a> : null}
                        </div>
                      </div>
                      <button
                        className="btn primary"
                        disabled={!searchSeriesId.trim()}
                        title={!searchSeriesId.trim() ? t('identify.needId') : t('identify.title')}
                        onClick={() => applyFromSearch(r)}
                      >{t('identify')}</button>
                    </div>
                  ))}
                </div>
              )}
            </div>
          </>
        )}

        {tab === 'servers' && SERVERS.map((s) => (
          <div className="card" key={s}>
            <h2>{SERVER_LABEL[s]}</h2>
            <p className="desc">{t('ms.desc', { s })}</p>
            <div className="grid">
              <Field label={t('f.baseUri')}><Text value={getPath(draft, [s, 'baseUri'])} onChange={(v) => upd([s, 'baseUri'], v)} /></Field>
              {s === 'komga' && (
                <>
                  <Field label={t('f.komgaUser')}><Text value={getPath(draft, [s, 'komgaUser'])} onChange={(v) => upd([s, 'komgaUser'], v)} /></Field>
                  <Field label={t('f.komgaPassword')} hint={t('sensitive')}><Text password value={getPath(draft, [s, 'komgaPassword']) ?? ''} onChange={(v) => upd([s, 'komgaPassword'], v)} placeholder="********" /></Field>
                  <Field label={t('f.komgaApiKey')} hint={t('sensitive')}><Text password value={getPath(draft, [s, 'komgaApiKey']) ?? ''} onChange={(v) => upd([s, 'komgaApiKey'], v)} placeholder={t('notConfigured')} /></Field>
                </>
              )}
              {s === 'kavita' && (
                <Field label={t('f.apiKey')} hint={t('sensitive')}><Text password value={getPath(draft, [s, 'apiKey']) ?? ''} onChange={(v) => upd([s, 'apiKey'], v)} placeholder={t('notConfigured')} /></Field>
              )}
              {s === 'stump' && (
                <>
                  <Field label={t('f.username')}><Text value={getPath(draft, [s, 'username'])} onChange={(v) => upd([s, 'username'], v)} /></Field>
                  <Field label={t('f.password')} hint={t('sensitive')}><Text password value={getPath(draft, [s, 'password']) ?? ''} onChange={(v) => upd([s, 'password'], v)} placeholder="********" /></Field>
                  <Field label={t('f.apiKey')} hint={t('sensitive')}><Text password value={getPath(draft, [s, 'apiKey']) ?? ''} onChange={(v) => upd([s, 'apiKey'], v)} placeholder={t('notConfigured')} /></Field>
                </>
              )}
            </div>
            <div className="grid">
              <SwitchField label={t('f.eventListenerEnabled')} value={getPath(draft, [s, 'eventListener', 'enabled'])} onChange={(v) => upd([s, 'eventListener', 'enabled'], v)} />
              <Field label={t('f.metadataLibraryFilter')}><MultiSelect value={getPath(draft, [s, 'eventListener', 'metadataLibraryFilter']) ?? []} options={(libs[s] ?? []).map((l) => ({ label: `${l.name ?? l.id}`, value: l.id }))} onChange={(v) => upd([s, 'eventListener', 'metadataLibraryFilter'], v)} /></Field>
              <Field label={t('f.metadataSeriesExcludeFilter')}><MultiSelect value={getPath(draft, [s, 'eventListener', 'metadataSeriesExcludeFilter']) ?? []} options={(libs[s] ?? []).map((l) => ({ label: `${l.name ?? l.id}`, value: l.id }))} onChange={(v) => upd([s, 'eventListener', 'metadataSeriesExcludeFilter'], v)} /></Field>
            </div>
          </div>
        ))}

        {tab === 'providers' && (
          <>
            <div className="card">
              <h2>{t('providers.title')}</h2>
              <p className="desc">{t('providers.desc')}</p>
              <div className="grid" style={{ gridTemplateColumns: '1fr 1fr' }}>
                <Field label={t('f.nameMatchingMode')}>
                  <select value={draft.metadataProviders?.nameMatchingMode ?? 'CLOSEST_MATCH'} onChange={(e) => upd(['metadataProviders', 'nameMatchingMode'], e.target.value)}>
                    {NAME_MODES.map((m) => <option key={m} value={m}>{t('mode.' + m)}</option>)}
                  </select>
                </Field>
                <Field label={t('f.malClientId')} hint={t('sensitive')}><Text password value={draft.metadataProviders?.malClientId?.includes('*') ? '' : (draft.metadataProviders?.malClientId ?? '')} onChange={(v) => upd(['metadataProviders', 'malClientId'], v)} placeholder="********" /></Field>
                <Field label={t('f.comicVineClientId')} hint={t('sensitive')}><Text password value={draft.metadataProviders?.comicVineClientId?.includes('*') ? '' : (draft.metadataProviders?.comicVineClientId ?? '')} onChange={(v) => upd(['metadataProviders', 'comicVineClientId'], v)} placeholder="********" /></Field>
                <Field label={t('f.bangumiToken')} hint={t('sensitive')}><Text password value={draft.metadataProviders?.bangumiToken?.includes('*') ? '' : (draft.metadataProviders?.bangumiToken ?? '')} onChange={(v) => upd(['metadataProviders', 'bangumiToken'], v)} placeholder={t('ph.nsfw')} /></Field>
                <Field label={t('f.comicVineSearchLimit')}><TriText value={getPath(draft, ['metadataProviders', 'comicVineSearchLimit'])} onChange={(v) => upd(['metadataProviders', 'comicVineSearchLimit'], v === null ? null : (Number(v) || 0))} placeholder={t('ph.ex20')} /></Field>
                <Field label={t('f.comicVineIssueName')}><TriText value={getPath(draft, ['metadataProviders', 'comicVineIssueName'])} onChange={(v) => upd(['metadataProviders', 'comicVineIssueName'], v)} placeholder={t('blankClears')} /></Field>
                <Field label={t('f.comicVineIdFormat')}><TriText value={getPath(draft, ['metadataProviders', 'comicVineIdFormat'])} onChange={(v) => upd(['metadataProviders', 'comicVineIdFormat'], v)} placeholder={t('blankClears')} /></Field>
              </div>
              <div className="card sub" style={{ marginTop: 10 }}>
                <h3>{t('oauth.title')}</h3>
                <p className="desc">{t('oauth.desc')}</p>
                <div className="row" style={{ flexWrap: 'wrap', gap: 8 }}>
                  {OAUTH_PROVIDERS.map((p) => {
                    const st = oauth[p];
                    const label = p === 'mal' ? 'MyAnimeList' : p === 'bangumi' ? 'Bangumi' : p === 'mangabaka' ? 'MangaBaka' : 'AniList';
                    return (
                      <span className="badge" key={p} style={{ display: 'inline-flex', alignItems: 'center', gap: 6 }}>
                        <b>{label}</b>
                        {st?.logged_in ? (
                          <>
                            <span
                              className="muted"
                              style={{ color: 'var(--ok)', fontWeight: 600 }}
                              title={t('oauth.loggedInAs', { u: st.username ?? '' })}
                            >✓ {st.username ?? ''}</span>
                            <button className="btn" onClick={() => oauthLogout(p)}>{t('oauth.logout')}</button>
                          </>
                        ) : (
                          <>
                            <span className="muted" title={t('oauth.notLoggedIn')}>○</span>
                            <a className="btn" href={`/api/oauth/${p}/start`}>{t('oauth.login')}</a>
                          </>
                        )}
                      </span>
                    );
                  })}
                  {oauthBusy ? <span className="muted" style={{ fontSize: 12 }}>{t('oauth.checking')}</span> : null}
                </div>
              </div>
              <div className="row" style={{ marginTop: 8 }}>
                <span className="badge">{t('ov.mangaBakaChecksum', { v: draft.metadataProviders?.mangaBakaDatabase?.downloadTimestamp ?? t('db.undownloaded'), c: draft.metadataProviders?.mangaBakaDatabase?.checksum?.slice(0, 12) ?? '-' })}</span>
                <button className="btn" disabled={!!dbBusy} onClick={() => runDbDownload('manga-baka')}>{dbBusy === 'manga-baka' ? t('db.downloading') : t('db.downloadDb')}</button>
                <span className="badge">{t('ov.bookWalkerDb', { v: draft.metadataProviders?.bookWalkerDownloadDate ?? t('db.undownloaded') })}</span>
                <button className="btn" disabled={!!dbBusy} onClick={() => runDbDownload('book-walker')}>{dbBusy === 'book-walker' ? t('db.downloading') : t('db.downloadDb')}</button>
                <span className="badge">{t('ov.bangumiDb', { v: draft.metadataProviders?.bangumiDatabase?.downloadTimestamp ?? t('db.undownloaded') })}</span>
                <button className="btn" disabled={!!dbBusy} onClick={() => runDbDownload('bangumi')}>{dbBusy === 'bangumi' ? t('db.downloading') : t('db.downloadDb')}</button>
                <span className="badge">{t('ov.ehentaiDb', { v: draft.metadataProviders?.ehentaiDatabase?.downloadTimestamp ?? t('db.undownloaded') })}</span>
                <button className="btn" disabled={!!dbBusy} onClick={() => runDbDownload('ehentai')}>{dbBusy === 'ehentai' ? t('db.downloading') : t('db.downloadDb')}</button>
              </div>
              {dbBusy && dbProg?.kind === dbBusy && (
                <div className="db-prog">
                  <div className="ev-progress"><div className="ev-progress-bar" style={{ width: `${dbProg.pct}%` }} /><span className="ev-progress-text">{dbProg.pct}%</span></div>
                  {dbProg.info ? <span className="muted" style={{ fontSize: 12 }}>{dbProg.info}</span> : null}
                </div>
              )}
              {dbErr && <div className="toast err">{t('db.err', { msg: dbErr.msg })}</div>}
              <p className="desc" style={{ marginTop: 6 }}>{t('db.desc')}</p>
            </div>
            <ProviderList draft={draft} upd={upd} base={['metadataProviders', 'defaultProviders']} expanded={expanded} setExpanded={setExpanded} />
          </>
        )}

        {tab === 'metadata' && SERVERS.map((s) => {
          const b = ['metadataUpdate', 'default'];
          const mu = getPath(draft, [s, ...b]) ?? {};
          return (
            <div className="card" key={s}>
              <h2>{t('md.defaultTitle', { s: SERVER_LABEL[s] })}</h2>
              <p className="desc">{t('md.desc', { s })}</p>
              <MetadataUpdateForm s={s} b={b} mu={mu} draft={draft} upd={upd} />
            </div>
          );
        })}

        {tab === 'library' && (
          <div className="card">
            <h2>{t('lib.title')}</h2>
            <p className="desc">{t('lib.desc', { id: '{id}' })}</p>
            <div className="row">
              <select value={scopeServer} onChange={(e) => { setScopeServer(e.target.value as ServerKind); setScopeLib(''); }}>
                {SERVERS.map((s) => <option key={s} value={s}>{s}</option>)}
              </select>
              <select value={scopeLib} onChange={(e) => {
                setScopeLib(e.target.value);
              }}>
                <option value="">{t('selectLib')}</option>
                {libOptions.map((l) => <option key={l.id} value={l.id} title={`${l.name} (${l.id})`}>{l.name} ({l.id})</option>)}
              </select>
              <button className="btn" onClick={() => {
                const def = getPath(draft, [scopeServer, 'metadataUpdate', 'default']);
                if (def) upd([scopeServer, 'metadataUpdate', 'library', scopeLib], clone(def));
                else setMsg({ ok: false, text: t('lib.copyFail') });
              }}>{t('lib.copySkeleton')}</button>
              <button className="btn" onClick={() => upd([scopeServer, 'metadataUpdate', 'library', scopeLib], null)}>{t('lib.delMeta')}</button>
              {scopeHasProviders && (
                <>
                  <button className="btn" onClick={() => {
                    const def = getPath(draft, ['metadataProviders', 'defaultProviders']);
                    if (def) upd(['metadataProviders', 'libraryProviders', scopeLib], clone(def));
                    else setMsg({ ok: false, text: t('lib.copyFail') });
                  }}>{t('lib.copyProv')}</button>
                  <button className="btn" onClick={() => upd(['metadataProviders', 'libraryProviders', scopeLib], null)}>{t('lib.delProv')}</button>
                </>
              )}
            </div>
            <div className="row" style={{ marginTop: 8, flexWrap: 'wrap' }}>
              <span className="badge">{t('lib.metaBadge', { ids: Object.keys(getPath(orig, [scopeServer, 'metadataUpdate', 'library']) ?? {}).map((id) => `${((libs[scopeServer] ?? []).find((l: any) => l.id === id)?.name ?? id)} (${id})`).join(', ') || t('noneList') })}</span>
              <span className="badge">{t('lib.provBadge', { ids: Object.keys(getPath(orig, ['metadataProviders', 'libraryProviders']) ?? {}).map((id) => `${((libs[scopeServer] ?? []).find((l: any) => l.id === id)?.name ?? id)} (${id})`).join(', ') || t('noneList') })}</span>
            </div>
            <div className="card" style={{ background: 'var(--panel2)' }}>
              <h3 style={{ overflowWrap: 'anywhere' }}>{t('lib.metaTitle', { lib: scopeLib || t('lib.unselected') })}</h3>
              <p className="desc">{t('lib.metaDesc')}</p>
              {scopeLib.trim() ? (
                <MetadataUpdateForm s={scopeServer} b={['metadataUpdate', 'library', scopeLib]} mu={getPath(draft, [scopeServer, 'metadataUpdate', 'library', scopeLib]) ?? {}} draft={draft} upd={upd} />
              ) : (
                <p className="desc">{t('selectLib')}</p>
              )}
            </div>
            {scopeHasProviders && (
              <ProviderList
                draft={draft}
                upd={upd}
                base={['metadataProviders', 'libraryProviders', scopeLib]}
                expanded={expanded}
                setExpanded={setExpanded}
                desc={t('lib.provDesc', { lib: scopeLib })}
              />
            )}
          </div>
        )}

        {tab === 'notifications' && (
          <>
            <div className="card">
              <h2>{t('notif.urls')}</h2>
              <p className="desc">{t('notif.urlsDesc')}</p>
              <div className="grid">
                <Field label={t('f.webhooks')}><textarea rows={4} value={(getPath(draft, ['notifications', 'discord', 'webhooks']) ?? []).join('\n').includes('*') ? '' : (getPath(draft, ['notifications', 'discord', 'webhooks']) ?? []).join('\n')} onChange={(e) => upd(['notifications', 'discord', 'webhooks'], e.target.value.split('\n').map((x) => x.trim()).filter(Boolean))} placeholder="https://discord.com/api/webhooks/…" /></Field>
                <Field label={t('f.urls')}><textarea rows={4} value={(getPath(draft, ['notifications', 'apprise', 'urls']) ?? []).join('\n').includes('*') ? '' : (getPath(draft, ['notifications', 'apprise', 'urls']) ?? []).join('\n')} onChange={(e) => upd(['notifications', 'apprise', 'urls'], e.target.value.split('\n').map((x) => x.trim()).filter(Boolean))} placeholder="discord://… / gotify://…" /></Field>
                <SwitchField label={t('f.seriesCover')} value={getPath(draft, ['notifications', 'discord', 'seriesCover'])} onChange={(v) => upd(['notifications', 'discord', 'seriesCover'], v)} />
                <SwitchField label={t('f.seriesCover')} value={getPath(draft, ['notifications', 'apprise', 'seriesCover'])} onChange={(v) => upd(['notifications', 'apprise', 'seriesCover'], v)} />
              </div>
            </div>

            <div className="card">
              <h2>{t('notif.discordTpl')}</h2>
              <p className="desc">{t('notif.tplDesc')}</p>
              <div className="grid">
                <Field label="title"><Text value={dTplEdit?.title ?? ''} onChange={(v) => setDTplEdit((t: any) => ({ ...t, title: v }))} placeholder="$series.name" /></Field>
                <Field label="titleUrl"><Text value={dTplEdit?.titleUrl ?? ''} onChange={(v) => setDTplEdit((t: any) => ({ ...t, titleUrl: v }))} /></Field>
                <Field label="footer"><Text value={dTplEdit?.footer ?? ''} onChange={(v) => setDTplEdit((t: any) => ({ ...t, footer: v }))} /></Field>
              </div>
              <Field label="description"><textarea rows={3} style={{ width: '100%' }} value={dTplEdit?.description ?? ''} onChange={(v) => setDTplEdit((t: any) => ({ ...t, description: v.target.value }))} placeholder="$series.metadata.summary" /></Field>
              <h3 style={{ margin: '10px 0 4px' }}>{t('notif.fieldsTitle')}</h3>
              {(dTplEdit?.fields ?? []).map((f: any, i: number) => (
                <div className="row" key={i} style={{ gap: 4, marginBottom: 4 }}>
                  <input style={{ flex: 1, minWidth: 0 }} value={f.name ?? ''} placeholder="name" onChange={(e) => setDField(i, 'name', e.target.value)} />
                  <input style={{ flex: 2, minWidth: 0 }} value={f.value ?? ''} placeholder="value" onChange={(e) => setDField(i, 'value', e.target.value)} />
                  <ToggleRow label="inline" value={!!f.inline} className="inlinerow" onChange={(v) => setDField(i, 'inline', v)} />
                  <button className="btn" onClick={() => setDTplEdit((t: any) => ({ ...t, fields: (t.fields ?? []).filter((_: any, j: number) => j !== i) }))}>{t('remove')}</button>
                </div>
              ))}
              <div className="row">
                <button className="btn" onClick={() => setDTplEdit((t: any) => ({ ...t, fields: [...(t?.fields ?? []), { name: '', value: '', inline: false }] }))}>{t('addField')}</button>
                <button className="btn" onClick={() => setDTplEdit(clone(dTpl))} disabled={!dTpl}>{t('discard')}</button>
                <span style={{ flex: 1 }} />
                <button className="btn primary" disabled={!dTplEdit || notifBusy === 'discord-save'} onClick={() => notifAction('discord', 'save')}>{notifBusy === 'discord-save' ? t('saving') : t('notif.saveTpl')}</button>
                <button className="btn" disabled={!dTplEdit || notifBusy === 'discord-render'} onClick={() => notifAction('discord', 'render')}>{notifBusy === 'discord-render' ? t('saving') : t('notif.renderPreview')}</button>
                <button className="btn" disabled={!dTplEdit || notifBusy === 'discord-send'} onClick={() => notifAction('discord', 'send')}>{notifBusy === 'discord-send' ? t('saving') : t('notif.sendTest')}</button>
              </div>
            </div>

            <div className="card">
              <h2>{t('notif.appriseTpl')}</h2>
              <p className="desc">{t('notif.appriseTplDesc')}</p>
              <Field label="title"><Text value={aTplEdit?.title ?? ''} onChange={(v) => setATplEdit((t: any) => ({ ...t, title: v }))} placeholder="$series.name" /></Field>
              <Field label="body"><textarea rows={5} style={{ width: '100%' }} value={aTplEdit?.body ?? ''} onChange={(v) => setATplEdit((t: any) => ({ ...t, body: v.target.value }))} placeholder="$series.metadata.summary" /></Field>
              <div className="row">
                <button className="btn" onClick={() => setATplEdit(clone(aTpl))} disabled={!aTpl}>{t('discard')}</button>
                <span style={{ flex: 1 }} />
                <button className="btn primary" disabled={!aTplEdit || notifBusy === 'apprise-save'} onClick={() => notifAction('apprise', 'save')}>{notifBusy === 'apprise-save' ? t('saving') : t('notif.saveTpl')}</button>
                <button className="btn" disabled={!aTplEdit || notifBusy === 'apprise-render'} onClick={() => notifAction('apprise', 'render')}>{notifBusy === 'apprise-render' ? t('saving') : t('notif.renderPreview')}</button>
                <button className="btn" disabled={!aTplEdit || notifBusy === 'apprise-send'} onClick={() => notifAction('apprise', 'send')}>{notifBusy === 'apprise-send' ? t('saving') : t('notif.sendTest')}</button>
              </div>
            </div>

            <div className="card">
              <h2>{t('notif.ctx')}</h2>
              <p className="desc">{t('notif.ctxDesc')}</p>
              <textarea rows={14} style={{ width: '100%' }} value={notifCtx} onChange={(e) => setNotifCtx(e.target.value)} />
              <div className="row">
                <button className="btn" onClick={() => setNotifCtx(JSON.stringify(SAMPLE_CTX, null, 2))}>{t('notif.resetCtx')}</button>
              </div>
              {notifMsg ? <div className={`toast ${notifMsg.ok ? 'ok' : 'err'}`}>{notifMsg.text}</div> : null}
            </div>

            {renderOut && (
              <div className="card">
                <h2>{t('notif.renderTitle', { ch: renderOut.channel })}</h2>
                {renderOut.channel === 'discord' ? (
                  <div className="render-card">
                    <b>{renderOut.data?.title || t('noTitle')}</b>
                    {renderOut.data?.titleUrl ? <div><a href={renderOut.data.titleUrl} target="_blank" rel="noreferrer">{renderOut.data.titleUrl}</a></div> : null}
                    {renderOut.data?.description ? <pre className="render-body">{renderOut.data.description}</pre> : null}
                    {(renderOut.data?.fields ?? []).map((f: any, i: number) => (
                      <div className="row" key={i}>
                        <span className="badge">{f.name}</span>
                        <span>{f.value}</span>
                        {f.inline ? <span className="badge">inline</span> : null}
                      </div>
                    ))}
                    {renderOut.data?.footer ? <div className="render-footer">{renderOut.data.footer}</div> : null}
                  </div>
                ) : (
                  <div className="render-card">
                    <b>{renderOut.data?.title || t('noTitle')}</b>
                    <pre className="render-body">{renderOut.data?.body}</pre>
                  </div>
                )}
              </div>
            )}
          </>
        )}

        {tab === 'jobs' && (
          <>
            <div className="card">
              <h2>{t('jobs.trigger')}</h2>
              <p className="desc">{t('jobs.triggerDesc')}</p>
              <div className="row">
                <select value={jobServer} onChange={(e) => { setJobServer(e.target.value as ServerKind); setJobLib(''); }}>
                  {SERVERS.map((s) => <option key={s} value={s}>{s}</option>)}
                </select>
                <select value={jobLib} onChange={(e) => setJobLib(e.target.value)}>
                  <option value="">{t('selectLib')}</option>
                  {(libs[jobServer] ?? []).map((l: any) => <option key={l.id} value={l.id} title={`${l.name} (${l.id})`}>{l.name} ({l.id})</option>)}
                </select>
                <input style={{ width: 240 }} value={jobSeriesId} placeholder={t('ph.seriesIdReq')} onChange={(e) => setJobSeriesId(e.target.value)} />
                <ToggleRow label={t('jobs.removeComicInfo')} value={removeComicInfo} className="jobrow" onChange={(v) => setRemoveComicInfo(v)} />
              </div>
              <div className="row" style={{ marginTop: 8 }}>
                <button className="btn primary" onClick={() => runTrigger('matchSeries')}>{t('jobs.matchSeries')}</button>
                <button className="btn" onClick={() => runTrigger('matchLib')}>{t('jobs.matchLib')}</button>
                <button className="btn" onClick={() => runTrigger('resetSeries')}>{t('jobs.resetSeries')}</button>
                <button className="btn" onClick={() => runTrigger('resetLib')}>{t('jobs.resetLib')}</button>
              </div>
            </div>

            <div className="card">
              <h2>{t('jobs.list')}</h2>
              <div className="row">
                <select value={jobStatus} onChange={(e) => { setJobStatus(e.target.value); setTimeout(refreshJobs, 0); }}>
                  <option value="">{t('allStatus')}</option>
                  <option value="RUNNING">RUNNING</option>
                  <option value="COMPLETED">COMPLETED</option>
                  <option value="FAILED">FAILED</option>
                </select>
                <button className="btn" onClick={refreshJobs}>{t('refresh')}</button>
                <button className="btn" onClick={clearJobs}>{t('clearAll')}</button>
                <span className="badge">{t('count', { n: jobs.length })}</span>
              </div>
              {jobs.length === 0 ? <p className="desc">{t('jobs.empty')}</p> : (
                <table className="job-table">
                  <thead>
                    <tr><th>{t('th.status')}</th><th>{t('th.series')}</th><th>{t('th.job')}</th><th>{t('th.message')}</th><th>{t('th.start')}</th><th>{t('th.end')}</th></tr>
                  </thead>
                  <tbody>
                    {jobs.map((j) => (
                      <tr key={j.id} className={activeJobId === j.id ? 'active' : ''} onClick={() => setActiveJobId(j.id)}>
                        <td><span className={j.status === 'COMPLETED' ? 'badge ok' : j.status === 'FAILED' ? 'badge bad' : 'badge warn'}>{j.status}</span></td>
                        <td className="mono">{String(j.seriesId ?? '').slice(0, 12)}</td>
                        <td className="mono" title={j.id}>{String(j.id).slice(0, 8)}</td>
                        <td>{j.message ?? ''}</td>
                        <td className="mono">{j.startedAt ? new Date(j.startedAt).toLocaleTimeString() : ''}</td>
                        <td className="mono">{j.finishedAt ? new Date(j.finishedAt).toLocaleTimeString() : ''}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </div>

            <div className="card">
              <div className="row">
                <h2 style={{ margin: 0 }}>{t('jobs.events')} {activeJobId ? `#${activeJobId.slice(0, 8)}` : ''}</h2>
                {activeJobId ? <button className="btn" onClick={() => setActiveJobId('')}>{t('close')}</button> : null}
              </div>
              {activeJobId ? <JobEventsFeed id={activeJobId} /> : <p className="desc">{t('jobs.eventsDesc')}</p>}
            </div>
          </>
        )}

        {tab === 'patch' && (
          <div className="card">
            <h2>{t('patch.title')}</h2>
            <p className="desc">{t('patch.desc')}</p>
            {!dirty && !scopeLib ? <div className="toast ok">{t('noChange')}</div> : null}
            <pre className="preview">{JSON.stringify(patch ?? {}, null, 2)}</pre>
            <h2>{t('yaml')}</h2>
            <pre className="preview">{YAML.stringify(patch ?? {})}</pre>
            <div className="row">
              <button className="btn" onClick={() => navigator.clipboard?.writeText(JSON.stringify(patch ?? {}, null, 2))}>{t('copyJson')}</button>
              <button className="btn primary" onClick={save} disabled={saving}>{t('save')}</button>
            </div>
          </div>
        )}
      </div>
    </>
  );
}
