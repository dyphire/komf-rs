// 深比较 + 增量 diff：只把改过的字段放进 PATCH body。
// 规则：
// - 凭据类空字符串视为"未改"(后端 GET 不返回密码，空=保持)。
// - undefined 的 key 直接省略；需要清空时由调用方显式设 null。
export function deepEqual(a: any, b: any): boolean {
  if (a === b) return true;
  if (typeof a !== typeof b) return false;
  if (a === null || b === null) return a === b;
  if (Array.isArray(a) || Array.isArray(b)) {
    if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length) return false;
    return a.every((v, i) => deepEqual(v, b[i]));
  }
  if (typeof a === 'object') {
    const ka = Object.keys(a);
    const kb = Object.keys(b);
    if (ka.length !== kb.length) return false;
    return ka.every((k) => deepEqual(a[k], b[k]));
  }
  return false;
}

/** orig=GET 原值, draft=表单值；返回 undefined 表示无改动。 */
export function diff(orig: any, draft: any): any {
  if (deepEqual(orig, draft)) return undefined;
  if (typeof draft !== 'object' || draft === null) return draft;
  if (typeof orig !== 'object' || orig === null) return draft;
  if (Array.isArray(draft)) return draft; // 数组整体替换(通知 url 列表走索引合并由后端处理)
  const out: any = {};
  for (const k of Object.keys(draft)) {
    // 空字符串凭据=保持，不发送
    if (draft[k] === '' && (k.toLowerCase().includes('password') || k.toLowerCase().includes('apikey') || k === 'apiKey' || k === 'komgaApiKey' || k === 'ipbMemberId' || k === 'ipbPassHash' || k === 'malClientId' || k === 'bangumiToken'))
      continue;
    if (!(k in orig)) {
      if (draft[k] !== undefined) out[k] = draft[k];
      continue;
    }
    const d = diff(orig[k], draft[k]);
    if (d !== undefined) out[k] = d;
  }
  return Object.keys(out).length ? out : undefined;
}

/** 把 draft 中用户清空想"删除按库覆盖"的 key 标 null 的辅助不在此做，由页面显式构造。 */
export function isEmptyPatch(p: any): boolean {
  return p === undefined || (typeof p === 'object' && p !== null && Object.keys(p).length === 0);
}
