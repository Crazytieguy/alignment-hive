declare module 'claude-code' {
  interface PluginState {
    'tldr': {
      /** Collapsing on (mirrors $.store 'enabled', which persists across sessions). */
      isOn: boolean
      /** By message id: the TL;DR, '' while none. */
      line: StateFamily<string>
      /** By message id: the person pressed [ more ]. */
      isExpanded: StateFamily<boolean>
      /** The person has used [ more ], [ less ] or /tldr (mirrors $.store 'learned'). */
      isLearned: boolean
      /** By message id: its row was stored, so the drawn text is whole. */
      isFinal: StateFamily<boolean>
    }
  }
}
