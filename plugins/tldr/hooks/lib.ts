// Pure helpers, tested with `claude plugin test`.

export const GUIDELINES = `You write the one sentence a reader sees in place of a long message from an AI assistant; the full message stays one click away. The reader asked the assistant for something, maybe hours of work ago, and wants to know how it went.

- One plain sentence, under 30 words, no prefix. Write as the assistant, to the reader: its "I" stays "I", the reader is "you".
- Lead with the outcome: the answer, result or finding, keeping the message's hedges, tense and who did what.
- Mention a question or request only if the message actually puts one to the reader, in its own words. Never comment on the message or on what it doesn't ask.
- If the reader must answer or decide and one sentence can't carry what they need, end by saying what to look at in the full message.

Reply with the sentence only.`

/** Long enough to collapse: over 100 words and more than one non-blank line. */
export function isLong(text: string): boolean {
  return wordCount(text) > 100 && text.split('\n').filter(l => l.trim()).length > 1
}

export function wordCount(text: string): number {
  return text.split(/\s+/).filter(Boolean).length
}

/** One line from the model's answer: trimmed, a stray prefix removed, newlines folded. */
export function lineFor(answer: string): string {
  return answer
    .trim()
    .replace(/^(tl;?dr|summary|one sentence|in one sentence)\s*:\s*/i, '')
    .replace(/\s*\n+\s*/g, ' ')
}

/** Parse `/tldr` arguments. */
export function parseArgs(args: string): 'toggle' | 'on' | 'off' | 'more' | 'less' | 'help' {
  const a = args.trim().toLowerCase()
  if (a === '') return 'toggle'
  if (a === 'on' || a === 'off' || a === 'more' || a === 'less') return a
  return 'help'
}
