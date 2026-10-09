// Pure helpers, tested with `claude plugin test`.

export const GUIDELINES = `The user's screen will show the message below, an AI assistant's message to them, collapsed to one sentence; the full message stays one click away. Write that sentence in the assistant's voice, to the user: its "I" stays "I", the user is "you".

- One plain sentence, aim for under 30 words, no prefix.
- Lead with its main point, keeping its hedges and status (proposed vs done, verified vs not).
- If it puts a question or decision to the user that one sentence can't carry, end by saying what to look at in the full message.

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
