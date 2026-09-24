/** Page time formats. Self-contained: the page runs this same source in the viewer's zone, the renderer in UTC for the fallback text. */
export function reviewTimes(zone?: string) {
  const parts = (iso: string, options: Intl.DateTimeFormatOptions): Record<string, string> =>
    Object.fromEntries(new Intl.DateTimeFormat('en-US', { timeZone: zone, hourCycle: 'h23', ...options }).formatToParts(new Date(iso)).map((part) => [part.type, part.value]));
  const moment = (iso: string) => {
    const p = parts(iso, { year: 'numeric', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });
    return { year: p.year, month: p.month, day: p.day, time: `${p.hour}:${p.minute}` };
  };
  /** "Sep 5 03:16" */
  const stamp = (iso: string) => { const m = moment(iso); return `${m.month} ${m.day} ${m.time}`; };
  return {
    stamp,
    /** "Sep 8" */
    day: (iso: string) => { const m = moment(iso); return `${m.month} ${m.day}`; },
    /** An ask's day and time apart, so the page shows the day only where it changes among the rows it shows; `key` compares days. */
    dayTime: (iso: string) => { const m = moment(iso); return { day: `${m.month} ${m.day}`, time: m.time, key: `${m.year}-${m.month}-${m.day}` }; },
  };
}
