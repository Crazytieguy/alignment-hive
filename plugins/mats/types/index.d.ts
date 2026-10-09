declare module 'claude-code' {
  interface PluginState {
    // `notices`: the check's rows, hooks/notice-band.tsx's Notice as parsed JSON.
    mats: { notices: unknown[]; dismissed: string[] }
  }
}
