/**
 * Consent window computation for determining session visibility.
 *
 * A session is visible to readers only if its lastModified timestamp falls
 * within a consent window for BOTH global and project consent layers.
 *
 * Window rules:
 * - First consent is retroactive: window starts at 0 (covers legacy data)
 * - Subsequent consents start at their consentedAt time (gap sessions excluded)
 * - Revocations close the current window at their consentedAt time
 * - A currently-active consent has end = Infinity
 */

export interface ConsentEvent {
  sessionSharing: boolean;
  consentedAt: number;
}

export interface ConsentWindow {
  start: number;
  end: number; // Infinity if currently active
}

/**
 * Compute consent windows from an event log.
 *
 * Events are sorted by time. The first opt-in opens a window at time 0
 * (retroactive for legacy data). Subsequent opt-ins open windows at their
 * consentedAt time. Opt-outs close the current window.
 */
export function computeConsentWindows(
  events: ConsentEvent[],
): ConsentWindow[] {
  const sorted = [...events].sort((a, b) => a.consentedAt - b.consentedAt);
  const windows: ConsentWindow[] = [];
  let isOpen = false;
  let isFirst = true;

  for (const event of sorted) {
    if (event.sessionSharing && !isOpen) {
      windows.push({
        start: isFirst ? 0 : event.consentedAt,
        end: Infinity,
      });
      isOpen = true;
      isFirst = false;
    } else if (!event.sessionSharing && isOpen) {
      // Close the current window
      windows[windows.length - 1].end = event.consentedAt;
      isOpen = false;
    }
  }

  return windows;
}

/**
 * Check if a timestamp falls within any consent window.
 */
export function isInConsentWindow(
  timestamp: number,
  windows: ConsentWindow[],
): boolean {
  return windows.some((w) => timestamp >= w.start && timestamp < w.end);
}
