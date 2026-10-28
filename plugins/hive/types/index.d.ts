// `hive notices <session id>` for one session: its rows are hooks/notice-band.tsx's Notice as
// parsed JSON; `isSessionKnown` false while the CLI has not discovered the session yet;
// `fetchedAt` the clock's time of the answer.
export type HiveNotices = { sessionId: string; rows: unknown[]; isSessionKnown: boolean; fetchedAt: number }

// A row whose button's command is running, and what the row says meanwhile.
export type HiveBusy = { row: string; label: string }

// The session whose info rows folded into the desktop's footer label after its first turn, and
// whether the person has opened them in the band again.
export type HiveFolded = { sessionId: string; isOpen: boolean }

// What automatic recall gave one prompt, by the prompt's transcript row: the entries Claude read
// with it and how far back recall read, or that it found none or did not finish.
export type HiveRecalled =
  | { entries: Array<{ loc: string; label: string; text: string; date: string; context: string }>; from: string }
  | 'none'
  | 'failed'
  | null

declare module 'claude-code' {
  interface PluginState {
    hive: {
      recalled: StateFamily<HiveRecalled>
      isRecallOpen: StateFamily<boolean>
      notices: HiveNotices | null
      dismissed: string[]
      promptedSession: string | null
      busy: HiveBusy | null
      isReviewing: boolean
      folded: HiveFolded | null
    }
  }
}
