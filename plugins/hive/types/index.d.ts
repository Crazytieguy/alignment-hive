// `hive notices <session id>` for one session: its rows are hooks/notice-band.tsx's Notice as
// parsed JSON; `isSessionKnown` false while the CLI has not discovered the session yet;
// `fetchedAt` the clock's time of the answer.
export type HiveNotices = { sessionId: string; rows: unknown[]; isSessionKnown: boolean; fetchedAt: number }

// A row whose button's command is running, and what the row says meanwhile.
export type HiveBusy = { row: string; label: string }

// The session whose info rows folded into the desktop's footer label after its first turn, and
// whether the person has opened them in the band again.
export type HiveFolded = { sessionId: string; isOpen: boolean }

declare module 'claude-code' {
  interface PluginState {
    hive: {
      notices: HiveNotices | null
      dismissed: string[]
      promptedSession: string | null
      busy: HiveBusy | null
      isReviewing: boolean
      folded: HiveFolded | null
    }
  }
}
